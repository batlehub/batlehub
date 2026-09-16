#!/usr/bin/env bash
# Fill `perf-apk` to a target index size, so scenario 13 measures a publish into
# an index of a known shape rather than into whatever the last run left behind.
#
# Usage: seed_apk.sh [BASE_URL] [COUNT]
#
# The packages are built here, in the v2 container, for the reason the heavy
# suite builds its own: `apk mkpkg` writes the v3 (ADB) format, which a v2
# `APKINDEX` cannot describe (RFC 0026 §13). The layout is not negotiable and
# every part of it was learned from a client refusing an earlier draft:
#
#   one stream   a `.apk` is one tar stream split across gzip members, so only
#                the last member carries the end-of-archive marker
#   `-b 1`       no 10 KiB record padding
#   no `./`      apk 3 refuses a data member with a `.` root entry
#   `datahash`   sha256 of the *compressed* data member; apk 3 refuses a v2
#                package without it
set -euo pipefail

BASE_URL="${1:-http://localhost:8080}"
COUNT="${2:-1000}"
REGISTRY="${BATLEHUB_APK_REGISTRY:-perf-apk}"
TOKEN="${BATLEHUB_ADMIN_TOKEN:-perf-admin-token}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

build() { # name version dest
  local name="$1" version="$2" dest="$3"
  rm -rf "$WORK/b"
  mkdir -p "$WORK/b/control" "$WORK/b/data/usr/share/$name"
  printf 'perf seed\n' > "$WORK/b/data/usr/share/$name/hello.txt"
  ( cd "$WORK/b/data" && tar -cf - -b 1 --format=ustar --owner=0 --group=0 \
      --mtime=@1700000000 usr | gzip -n ) > "$WORK/b/data.gz"
  local datahash
  datahash="$(sha256sum "$WORK/b/data.gz" | cut -d' ' -f1)"
  cat > "$WORK/b/control/.PKGINFO" <<PKGINFO
pkgname = $name
pkgver = $version
arch = x86_64
size = 10
builddate = 1700000000
pkgdesc = batlehub perf seed
license = MIT
datahash = $datahash
PKGINFO
  ( cd "$WORK/b/control" && tar -cf - -b 1 --format=ustar --owner=0 --group=0 \
      --mtime=@1700000000 .PKGINFO ) > "$WORK/b/control.tar"
  local len
  len="$(wc -c < "$WORK/b/control.tar")"
  head -c "$(( len - 1024 ))" "$WORK/b/control.tar" | gzip -n > "$dest"
  cat "$WORK/b/data.gz" >> "$dest"
}

echo "seeding $COUNT packages into $REGISTRY at $BASE_URL"
start="$(date +%s)"
for i in $(seq 1 "$COUNT"); do
  name="$(printf 'perf-apk-%05d' "$i")"
  build "$name" "1.0.0-r0" "$WORK/pkg.apk"
  status="$(curl -sS -o /dev/null -w '%{http_code}' -X PUT \
    "$BASE_URL/proxy/$REGISTRY/apk/upload" \
    -H "Authorization: Bearer $TOKEN" --data-binary @"$WORK/pkg.apk")"
  if [[ "$status" != "201" && "$status" != "409" ]]; then
    echo "publish of $name answered $status" >&2
    exit 1
  fi
  if (( i % 100 == 0 )); then
    echo "  $i/$COUNT  ($(( $(date +%s) - start ))s)"
  fi
done

size="$(curl -sS -o /dev/null -w '%{size_download}' \
  "$BASE_URL/proxy/$REGISTRY/apk/x86_64/APKINDEX.tar.gz" \
  -H "Authorization: Bearer $TOKEN")"
echo "done in $(( $(date +%s) - start ))s; APKINDEX.tar.gz is now $size bytes"
