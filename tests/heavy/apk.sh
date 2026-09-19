#!/usr/bin/env bash
# Heavy Alpine apk integration test — the real `apk` against an `apk` registry,
# both generations, through the wire tap (RFC 0026 §6.8).
#
# A suite of its own rather than a `closed_world.sh` phase, for `cargo`'s reason
# one format over: the client *is* the package manager of the distribution this
# kind serves, so the suite bootstraps it out of the very tree it then proxies.
#
# **Both generations, neither skipped.** Alpine ships `apk-tools-static` for
# each — 2.14.10 on v3.22 and 3.0.8 on v3.23/v3.24 — so both run as static
# binaries under `--root`, with no rootfs and no user namespace. That matters:
# a user namespace is blocked by AppArmor on the GitHub image, so the obvious
# minirootfs plan would have meant a reported skip in the one suite whose whole
# purpose is to make a skip impossible.
#
# What the transcript has to show, per generation:
#
#   Relay    `apk update` fetches APKINDEX.tar.gz *through the proxy* and
#            verifies Alpine's own signature on the relayed bytes. A filtered
#            index would fail here, which is why §4.4 relays it byte-exact.
#   Refuse   With a version blocked, the download is a 403 and the client fails
#            its transaction. apk 2.14 maps it through libfetch to "Permission
#            denied"; apk 3 prints "HTTP 403: Forbidden".
#   Recover  After the block lifts, the same cache directory fetches it.
#   Cache    A second fetch from a cold client cache is served from storage.
#   Credential  apk's only mechanism is HTTP Basic from userinfo in the URL.
#            The denied arm 403s *and the allowed arm succeeds* — without the
#            second half, the first proves only that an anonymous client is
#            anonymous.
#
# Run via `task test:apk-heavy` or directly. Needs network: dl-cdn.alpinelinux.org.
# Environment knobs: DATABASE_URL (required), HEAVY_PORT (8108),
# HEAVY_TAP_PORT (8118), COVERAGE, HEAVY_APK_PKG (busybox).

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init apk 8108 8118
heavy_need curl "curl"
heavy_need python3 "python3 (the wire tap)"
heavy_need tar "tar"
heavy_need openssl "openssl (the local repository's index signing key)"

REG="apk-$HEAVY_RUN"
AUTH_REG="apk-auth-$HEAVY_RUN"
LOCAL_REG="apk-local-$HEAVY_RUN"
KEY_NAME="heavy-$HEAVY_RUN@batlehub.test-5f3a1c2e.rsa.pub"
PKG="${HEAVY_APK_PKG:-busybox}"

# The two generations, each with the branch that ships it. Verified against the
# mirror: v3.21 has 2.14.6, v3.22 has 2.14.10, and v3.23/v3.24/latest-stable/
# edge all have 3.0.8. There is no v3.25 — the branch list stops at v3.24.
GEN2_BRANCH="v3.22"
GEN3_BRANCH="v3.24"

# The key the local repository's index is signed with, generated per run and
# never written to disk in the repository. The config reads it as
# `${APK_SIGNING_KEY_PEM}`; `\n` escapes rather than newlines because the
# loader expands `${VAR}` into the TOML *source*, and a raw newline inside a
# basic string is a parse error there.
APK_SIGNING_KEY_PEM="$(openssl genrsa 2048 2>/dev/null | sed ':a;N;$!ba;s/\n/\\n/g')"
[[ -n "$APK_SIGNING_KEY_PEM" ]] || heavy_fail "openssl genrsa produced nothing"
export APK_SIGNING_KEY_PEM

heavy_start_server tests/heavy/config.apk.toml
heavy_start_tap

# ── Bootstrapping the clients ────────────────────────────────────────────────
#
# Fetched from the CDN directly, not through the proxy: the suite needs a
# working client *before* it can assert anything about the proxy, and a
# bootstrap that went through the thing under test would make a proxy bug look
# like a missing client.

CDN="https://dl-cdn.alpinelinux.org/alpine"

# `-L` follows a redirect, and the CDN redirects; `--proto-redir` pins the
# redirect chain to HTTPS so a downgrade cannot slip a plain-HTTP hop into a
# download that goes straight into `tar`. A wrapper rather than the flags
# spliced in at each call site, for the reason `marketplace.sh` records: an
# array splat is opaque to the rule that checks for this, and a call site that
# names the wrapper cannot omit half of the pair.
fetch_https() {
  curl -fsSL --proto '=https' --proto-redir '=https' "$@"
  return $?
}

# apk_static_for <branch> <dest-dir> — unpack apk-tools-static and echo the
# path to the binary.
apk_static_for() {
  local branch="$1" dest="$2"
  mkdir -p "$dest"

  local listing="$HEAVY_WORK/listing-$branch.html"
  fetch_https "$CDN/$branch/main/x86_64/" -o "$listing" \
    || heavy_fail "could not list $branch/main/x86_64 on the CDN"

  local file
  file="$(grep -oE 'apk-tools-static-[0-9][^"]*\.apk' "$listing" | sort -u | head -1)" \
    || true
  [[ -n "$file" ]] || heavy_fail "no apk-tools-static package found in $branch/main/x86_64"

  fetch_https "$CDN/$branch/main/x86_64/$file" -o "$dest/apk-tools-static.apk" \
    || heavy_fail "could not download $file"

  # A .apk is concatenated gzip members; GNU tar walks them and extracts the
  # union, which is all that is needed to get the binary out.
  (cd "$dest" && tar -xzf apk-tools-static.apk 2>/dev/null) || true
  [[ -x "$dest/sbin/apk.static" ]] \
    || heavy_fail "apk.static was not where $file said it would be ($dest/sbin/apk.static)"

  printf '%s' "$dest/sbin/apk.static"
  return $?
}

# The Alpine signing keys, so a relayed index verifies. Taken from the same
# package apk itself installs them from.
alpine_keys() {
  local dest="$1"
  mkdir -p "$dest"
  local listing="$HEAVY_WORK/keys-listing.html"
  fetch_https "$CDN/$GEN2_BRANCH/main/x86_64/" -o "$listing" \
    || heavy_fail "could not list the CDN for alpine-keys"
  local file
  file="$(grep -oE 'alpine-keys-[0-9][^"]*\.apk' "$listing" | sort -u | head -1)"
  [[ -n "$file" ]] || heavy_fail "no alpine-keys package on the CDN"
  fetch_https "$CDN/$GEN2_BRANCH/main/x86_64/$file" -o "$HEAVY_WORK/alpine-keys.apk" \
    || heavy_fail "could not download $file"
  (cd "$HEAVY_WORK" && tar -xzf alpine-keys.apk 2>/dev/null) || true
  cp "$HEAVY_WORK"/usr/share/apk/keys/*.rsa.pub "$dest/" 2>/dev/null \
    || heavy_fail "alpine-keys carried no .rsa.pub files"
  return $?
}

KEYS="$HEAVY_WORK/keys"
alpine_keys "$KEYS"
heavy_log "Alpine keys installed: $(ls "$KEYS" | wc -l) public keys"

APK2="$(apk_static_for "$GEN2_BRANCH" "$HEAVY_WORK/apk2")"
APK3="$(apk_static_for "$GEN3_BRANCH" "$HEAVY_WORK/apk3")"
heavy_log "clients: 2.x = $("$APK2" --version 2>&1 | head -1), 3.x = $("$APK3" --version 2>&1 | head -1)"

# run_apk <binary> <root> <repo-line> <args...> — apk against its own root,
# keys, repositories file and cache, so the runner's own package database is
# never touched. Output lands in RUN_OUT; the exit code is returned.
RUN_OUT=""
run_apk() {
  local bin="$1" root="$2" repo="$3"
  shift 3
  mkdir -p "$root/etc/apk" "$root/cache"
  printf '%s\n' "$repo" > "$root/repositories"
  # `RUN_KEYS` as a one-call prefix swaps Alpine's keys for this instance's in
  # the local arm, where trusting only our own key is the point.
  #
  # No `--allow-untrusted` in any spelling. Refusing an unverifiable index is
  # the default in both generations, and the negative has no portable spelling:
  # 2.14 takes `--allow-untrusted` as a bare flag and exits with a usage error
  # on `--allow-untrusted=false`. Omitting it is the assertion — every index
  # this suite reads is verified, or the client fails.
  local -a common=(
    --root "$root" --arch x86_64 --keys-dir "${RUN_KEYS:-$KEYS}"
    --repositories-file "$root/repositories" --cache-dir "$root/cache"
  )

  # A `--root` with no database is not a root: both generations refuse to open
  # one ("Failed to open apk database"), and the only applet that creates one
  # is `add --initdb` — `--initdb` is not a global option in either. apk 3
  # additionally wants `--usermode`, because this runs as an ordinary user and
  # 3.0 refuses to create a database outside one without being told so. The
  # repository fetch it attempts on the way is not an assertion of this suite
  # and its failure is ignored; what matters is the database it leaves behind.
  if [[ ! -e "$root/lib/apk/db/installed" ]]; then
    local -a init=(add --initdb)
    [[ "$bin" == "$APK3" ]] && init+=(--usermode)
    "$bin" "${common[@]}" "${init[@]}" >"$HEAVY_WORK/initdb.log" 2>&1 || true
    [[ -e "$root/lib/apk/db/installed" ]] \
      || { cat "$HEAVY_WORK/initdb.log" >&2; heavy_fail "apk could not initialise a database under $root"; }
  fi

  RUN_OUT="$HEAVY_WORK/apk-out.$$.$RANDOM"
  "$bin" "${common[@]}" "$@" >"$RUN_OUT" 2>&1
  return $?
}

# assert_update_resolved <label> — what an `apk update` that really read an
# index looks like, in *each* generation's own words. They do not agree:
#
#   2.14  `OK: 5647 distinct packages available`
#   3.0   `0 unavailable, 0 stale; 5647 distinct packages available`
#
# so the shared assertion is the package count, plus the half apk 3 alone can
# say: a repository it could not read is `N unavailable`, and it reports that
# while exiting 0. Without the count a suite driving apk 3 through a broken
# proxy is green, which is the failure mode this whole file exists to prevent.
assert_update_resolved() {
  local label="$1" count
  count="$(grep -oE '[0-9]+ distinct packages available' "$RUN_OUT" \
    | grep -oE '^[0-9]+' | tail -1)"
  [[ -n "$count" && "$count" -gt 0 ]] \
    || { cat "$RUN_OUT" >&2; heavy_fail "[$label] apk update resolved no packages — the index did not verify"; }
  if grep -qE '[1-9][0-9]* unavailable' "$RUN_OUT"; then
    cat "$RUN_OUT" >&2
    heavy_fail "[$label] apk reported an unavailable repository while exiting 0"
  fi
  heavy_client_said "$RUN_OUT" '[0-9]+ distinct packages available'
  return $?
}

# ── The per-generation body ──────────────────────────────────────────────────

# generation <label> <binary> <branch>
generation() {
  local label="$1" bin="$2" branch="$3"
  local repo="$HEAVY_TAP_BASE/proxy/$REG/apk/$branch/main"
  local root="$HEAVY_WORK/root-$label"

  heavy_log "[$label] Relay — apk update through the proxy"
  heavy_mark "$label-update"
  run_apk "$bin" "$root/update" "$repo" update \
    || { cat "$RUN_OUT" >&2; heavy_fail "[$label] apk update failed against the proxy"; }
  assert_update_resolved "$label"
  # Assert on the wire, not on the exit code: a client that reached the CDN
  # directly would also print OK.
  heavy_wire_re_after "$label-update" \
    "GET /proxy/$REG/apk/$branch/main/x86_64/APKINDEX[.]tar[.]gz.* 200" \
    "[$label] the index was not fetched through the proxy, or was not a 200"

  # The version the branch actually carries, read from the client's own view.
  local version
  version="$(run_apk "$bin" "$root/update" "$repo" list "$PKG" >/dev/null 2>&1; \
             grep -oE "$PKG-[0-9][^ ]*" "$RUN_OUT" | head -1 | sed "s/^$PKG-//")" || true
  if [[ -z "$version" ]]; then
    # apk 3 spells `list` differently in places; fall back to the index.
    version="$(curl -fsS "$HEAVY_BASE/proxy/$REG/apk/$branch/main/x86_64/APKINDEX.tar.gz" \
      | python3 -c '
import sys,zlib,io,tarfile
b=sys.stdin.buffer.read(); off=0; text=""
while off < len(b):
    d=zlib.decompressobj(31); out=d.decompress(b[off:])
    used=len(b[off:])-len(d.unused_data)
    if used == 0: break
    try:
        t=tarfile.open(fileobj=io.BytesIO(out))
        f=t.extractfile("APKINDEX")
        if f: text=f.read().decode()
    except Exception: pass
    off += used
    if not d.unused_data: break
import os
want=os.environ["PKG"]; name=ver=None
for block in text.split("\n\n"):
    n=v=None
    for line in block.splitlines():
        if line.startswith("P:"): n=line[2:]
        elif line.startswith("V:"): v=line[2:]
    if n==want and v: print(v); break
' PKG="$PKG")" || true
  fi
  [[ -n "$version" ]] || heavy_fail "[$label] could not determine the $PKG version on $branch"
  heavy_log "[$label] $PKG is $version on $branch"

  heavy_log "[$label] Fetch — the package through the proxy"
  heavy_mark "$label-fetch"
  run_apk "$bin" "$root/fetch" "$repo" fetch --stdout "$PKG" \
    || { cat "$RUN_OUT" >&2; heavy_fail "[$label] apk fetch failed"; }
  heavy_wire_re_after "$label-fetch" \
    "GET /proxy/$REG/apk/$branch/main/x86_64/$PKG-$version[.]apk.* 200" \
    "[$label] the package was not fetched through the proxy"

  heavy_log "[$label] Refuse — the same fetch with the version blocked"
  heavy_block "$REG" "$PKG" "$version"
  heavy_mark "$label-blocked"
  if run_apk "$bin" "$root/blocked" "$repo" fetch --stdout "$PKG"; then
    cat "$RUN_OUT" >&2
    heavy_fail "[$label] apk fetched a blocked version — the download gate did not refuse it"
  fi
  heavy_client_said "$RUN_OUT" '(Permission denied|403|Forbidden|ERROR)'
  # The coordinate reached the gate: a 403 on the .apk, and the block was
  # enforced at the package because the index cannot be filtered.
  heavy_wire_re_after "$label-blocked" \
    "GET /proxy/$REG/apk/$branch/main/x86_64/$PKG-$version[.]apk.* 403" \
    "[$label] the blocked package was not refused with a 403"

  heavy_log "[$label] Recover — the block lifts, the same client fetches"
  heavy_unblock "$REG" "$PKG" "$version"
  heavy_mark "$label-recover"
  run_apk "$bin" "$root/blocked" "$repo" fetch --stdout "$PKG" \
    || { cat "$RUN_OUT" >&2; heavy_fail "[$label] apk could not fetch after the block lifted"; }
  heavy_wire_re_after "$label-recover" \
    "GET /proxy/$REG/apk/$branch/main/x86_64/$PKG-$version[.]apk.* 200" \
    "[$label] the unblocked package was not served"

  heavy_log "[$label] Credential — the boundary apk's URL-embedded Basic crosses"
  local anon_repo="$HEAVY_TAP_BASE/proxy/$AUTH_REG/apk/$branch/main"
  local auth_repo="http://ci-reader:heavy-apk-reader@127.0.0.1:$HEAVY_TAP_PORT/proxy/$AUTH_REG/apk/$branch/main"

  # The refusal is asserted on what the client *resolved*, not on its exit
  # code: apk 3 reports a repository it could not read as `1 unavailable` and
  # exits 0, so `if run_apk …; then fail; fi` would pass this arm for the
  # wrong reason on one of the two generations.
  heavy_mark "$label-anon"
  run_apk "$bin" "$root/anon" "$anon_repo" update || true
  if grep -qE '[1-9][0-9]* distinct packages available' "$RUN_OUT"; then
    cat "$RUN_OUT" >&2
    heavy_fail "[$label] an anonymous apk update resolved packages against a registry with no anonymous verbs"
  fi
  heavy_wire_re_after "$label-anon" \
    "GET /proxy/$AUTH_REG/apk/$branch/main/x86_64/APKINDEX[.]tar[.]gz.* 40[13]" \
    "[$label] the anonymous index request was not refused"

  # The half that makes the half above mean something.
  heavy_mark "$label-authed"
  run_apk "$bin" "$root/authed" "$auth_repo" update \
    || { cat "$RUN_OUT" >&2; heavy_fail "[$label] an authenticated apk update failed — apk's URL-embedded Basic credential did not reach the server"; }
  assert_update_resolved "$label authenticated"
  heavy_wire_re_after "$label-authed" \
    "GET /proxy/$AUTH_REG/apk/$branch/main/x86_64/APKINDEX[.]tar[.]gz.* 200" \
    "[$label] the authenticated index request did not succeed"
  heavy_log "[$label] credential boundary proven both ways"
  return $?
}

# ── The local half ──────────────────────────────────────────────────────────
#
# Step 5 of §6.8, and the half no other OS kind can have: the index served here
# is this instance's to write, so it is *filtered* at generation and signed with
# a key served on the key route. The client trusts that key and nothing else, so
# an index that verifies verifies against our RSA256 entry — not against
# Alpine's, whose keys are deliberately absent from this directory.

# apk_v2_fixture <name> <version> <dest> — a real `.apk` in the **v2** format,
# built here rather than by a client.
#
# `apk mkpkg` cannot do this: apk-tools 3.0.8's builder writes the v3 (ADB)
# container — first bytes `ADBd` — and nothing ships a v3 index to put such a
# package in. Every package on the mirror is still v2 and `abuild` (which needs
# an Alpine host) still writes v2, so the suite assembles the container itself.
#
# Four things about the layout, every one of them learned from a client
# refusing an earlier draft rather than from the format description:
#
#   one stream   A `.apk` is **one tar stream split across gzip members**, so
#                only the last member carries the end-of-archive marker. `tar`
#                writes one per invocation, so the control member's is cut off.
#   `-b 1`       GNU tar pads to a 20-block (10 KiB) record by default; a real
#                `.apk` has no such padding and the zero blocks read as the end
#                of the stream.
#   no `./`      `tar -c .` puts a `.` root entry in the archive. apk 2.14
#                tolerates it; apk 3.0.8 answers `file format is invalid or
#                inconsistent`. Real packages name `usr` directly.
#   `datahash`   sha256 of the **compressed** data member, verified against
#                `busybox-1.37.0-r20.apk`'s own line. apk 3 refuses a v2
#                package without it (`v2 package format error`); apk 2 does
#                not care. It also decides which rule the index's `C:` follows,
#                so the data member has to be built and hashed *before* the
#                control file that names it.
apk_v2_fixture() {
  local name="$1" version="$2" dest="$3"
  local src="$HEAVY_WORK/fixture-$name"
  rm -rf "$src"
  mkdir -p "$src/control" "$src/data/usr/share/$name"
  printf 'published by the batlehub heavy suite, run %s\n' "$HEAVY_RUN" \
    > "$src/data/usr/share/$name/hello.txt"
  local size
  size="$(wc -c < "$src/data/usr/share/$name/hello.txt")"

  # 1. The data member, and its digest.
  ( cd "$src/data" && tar -cf - -b 1 --format=ustar --owner=0 --group=0 \
      --mtime=@1700000000 usr | gzip -n ) > "$src/data.gz"
  local datahash
  datahash="$(sha256sum "$src/data.gz" | cut -d' ' -f1)"

  # 2. The control file that names it, then the control member — unterminated,
  #    because the data member continues the same tar stream.
  cat > "$src/control/.PKGINFO" <<PKGINFO
pkgname = $name
pkgver = $version
arch = x86_64
size = $size
builddate = $(date +%s)
pkgdesc = batlehub heavy apk fixture
license = MIT
datahash = $datahash
PKGINFO
  ( cd "$src/control" && tar -cf - -b 1 --format=ustar --owner=0 --group=0 \
      --mtime=@1700000000 .PKGINFO ) > "$src/control.tar"
  local tar_len eof=1024
  tar_len="$(wc -c < "$src/control.tar")"
  head -c "$(( tar_len - eof ))" "$src/control.tar" | gzip -n > "$dest"
  cat "$src/data.gz" >> "$dest"

  [[ -s "$dest" ]] || heavy_fail "could not build the v2 fixture $name-$version"
  return $?
}

# local_generation <label> <binary>
local_generation() {
  local label="$1" bin="$2"
  local name="heavy-$label-$HEAVY_RUN" version="1.0.0-r0"
  local file="$HEAVY_WORK/$label-fixture.apk"
  local root="$HEAVY_WORK/local-$label"
  local repo="$HEAVY_TAP_BASE/proxy/$LOCAL_REG/apk"
  local keys="$HEAVY_WORK/local-keys-$label"

  heavy_log "[$label] Local — publish a v2 package this suite built"
  apk_v2_fixture "$name" "$version" "$file"
  # Without `-f`, so a refusal arrives as its body and not as "curl: (22)".
  # The first run of this suite spent a round trip on a `502` whose message —
  # the one that named the format — `-f` had thrown away.
  local status
  status="$(curl -sS -o "$HEAVY_WORK/publish-$label.out" -w '%{http_code}' \
    -X PUT "$HEAVY_BASE/proxy/$LOCAL_REG/apk/upload" \
    -H "Authorization: Bearer $ADMIN_TOKEN" --data-binary @"$file")"
  [[ "$status" == "201" ]] \
    || { cat "$HEAVY_WORK/publish-$label.out" >&2; heavy_fail "[$label] the publish answered $status, expected 201"; }
  heavy_log "[$label] server said: $(cat "$HEAVY_WORK/publish-$label.out")"

  # The key, from the route that serves it, into a directory holding nothing
  # else. A near miss on the name is an *untrusted index* rather than an error
  # the client names, which is why the route serves it back verbatim.
  mkdir -p "$keys"
  curl -fsS "$HEAVY_BASE/proxy/$LOCAL_REG/apk/keys/$KEY_NAME" -o "$keys/$KEY_NAME" \
    || heavy_fail "[$label] the key route did not serve $KEY_NAME"
  grep -q "BEGIN PUBLIC KEY" "$keys/$KEY_NAME" \
    || heavy_fail "[$label] the key route served something that is not a public key"

  heavy_log "[$label] Local — apk update verifies the index this instance signed"
  heavy_mark "$label-local-update"
  RUN_KEYS="$keys" run_apk "$bin" "$root/update" "$repo" update \
    || { cat "$RUN_OUT" >&2; heavy_fail "[$label] apk update failed against the local repository — the RSA256 signature did not verify"; }
  assert_update_resolved "$label local"
  heavy_wire_re_after "$label-local-update" \
    "GET /proxy/$LOCAL_REG/apk/x86_64/APKINDEX[.]tar[.]gz.* 200" \
    "[$label] the local index was not fetched through the proxy, or was not a 200"

  heavy_log "[$label] Local — fetch the package, identity checked against the index"
  heavy_mark "$label-local-fetch"
  RUN_KEYS="$keys" run_apk "$bin" "$root/fetch" "$repo" fetch --stdout "$name" \
    || { cat "$RUN_OUT" >&2; heavy_fail "[$label] apk could not fetch the locally published package"; }
  heavy_wire_re_after "$label-local-fetch" \
    "GET /proxy/$LOCAL_REG/apk/x86_64/$name-$version[.]apk.* 200" \
    "[$label] the local package was not served"

  # The difference from the proxy half, and the reason this arm exists: a
  # blocked version disappears from a *listing* we generate, so the client
  # never asks for it and fails in its own solver instead of on a 403.
  heavy_log "[$label] Local — a blocked version leaves the regenerated index"
  heavy_block "$LOCAL_REG" "$name" "$version"
  heavy_mark "$label-local-blocked"
  RUN_KEYS="$keys" run_apk "$bin" "$root/blocked" "$repo" update \
    || { cat "$RUN_OUT" >&2; heavy_fail "[$label] apk update failed after the block — the regenerated index did not verify"; }
  if RUN_KEYS="$keys" run_apk "$bin" "$root/blocked" "$repo" fetch --stdout "$name"; then
    cat "$RUN_OUT" >&2
    heavy_fail "[$label] apk fetched a blocked package from the local repository"
  fi
  heavy_client_said "$RUN_OUT" '(unable to select|not found|no such package|ERROR)'
  # Nothing was even requested: the block is enforced in the listing here, not
  # at the download, so the transcript carries no request for the file.
  heavy_wire_re_after "$label-local-blocked" \
    "GET /proxy/$LOCAL_REG/apk/x86_64/APKINDEX[.]tar[.]gz.* 200" \
    "[$label] the regenerated index was not re-read after the block"
  if [[ "$(heavy_wire_count_after "$label-local-blocked" "GET /proxy/$LOCAL_REG/apk/x86_64/$name-$version[.]apk")" != "0" ]]; then
    heavy_fail "[$label] the client asked for the blocked package — it was still listed in the regenerated index"
  fi

  heavy_unblock "$LOCAL_REG" "$name" "$version"
  heavy_log "[$label] local repository proven: signed index, identity, block in the listing"
  return $?
}

generation "apk2" "$APK2" "$GEN2_BRANCH"
generation "apk3" "$APK3" "$GEN3_BRANCH"

local_generation "apk2" "$APK2"
local_generation "apk3" "$APK3"

# ── Cache — a second cold client is served from storage ──────────────────────

heavy_log "Cache — a cold client re-fetches what storage already holds"
BEFORE="$(curl -fsS "$HEAVY_BASE/metrics" \
  | awk '/^batlehub_artifact_cache_hits_total/ { total += $2 } END { print total + 0 }')"
heavy_mark "cache"
run_apk "$APK2" "$HEAVY_WORK/root-cold" \
  "$HEAVY_TAP_BASE/proxy/$REG/apk/$GEN2_BRANCH/main" fetch --stdout "$PKG" \
  || { cat "$RUN_OUT" >&2; heavy_fail "the cold client could not fetch"; }
AFTER="$(curl -fsS "$HEAVY_BASE/metrics" \
  | awk '/^batlehub_artifact_cache_hits_total/ { total += $2 } END { print total + 0 }')"
[[ "$AFTER" -gt "$BEFORE" ]] \
  || heavy_fail "batlehub_artifact_cache_hits_total did not move ($BEFORE → $AFTER): the second fetch went upstream"
heavy_log "cache hits: $BEFORE → $AFTER"

# ── The malformed-name edge ──────────────────────────────────────────────────
#
# Not a client assertion: no apk asks for this. It is the 400 the handler owes
# a request whose file name carries no `-r<digits>`, checked here because this
# is where a real server is running.

heavy_log "Edge — a .apk with no release suffix is a 400, not a guess"
STATUS="$(curl -s -o /dev/null -w '%{http_code}' \
  "$HEAVY_BASE/proxy/$REG/apk/$GEN2_BRANCH/main/x86_64/notaversion.apk")"
[[ "$STATUS" == "400" ]] \
  || heavy_fail "a malformed .apk name answered $STATUS, expected 400"

heavy_done APK-HEAVY-OK
