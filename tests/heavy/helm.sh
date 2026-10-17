#!/usr/bin/env bash
# Heavy Helm integration test — the real `helm`, against two real chart
# repositories through the proxy, and a ChartMuseum publish (RFC 0029 §6.10).
#
# Why a heavy suite when the in-process tests cover every route: a route test
# proves routing. Only the real client proves that `helm` accepts the index this
# server re-emits, resolves the rewritten URL, sends its basic auth to it, and
# verifies a `.prov` this server relayed.
#
# Two upstreams, because they differ in the one way that matters:
# charts.jetstack.io writes *relative* chart URLs and publishes `.prov` files;
# kubernetes.github.io/ingress-nginx writes *absolute* GitHub release URLs that
# answer with a `302` to a signed asset host.
#
#   1. repo-add  — `helm repo add` reads the index with basic auth; every URL
#                  in the served index is on the tap's host; no `oci://` left.
#   2. verify    — `helm pull --verify` fetches the archive *and* its `.prov`
#                  here, and helm reports the signature good: both byte-exact.
#   3. absolute  — an absolute-URL chart pulls with the client's egress denied:
#                  the `302` chain is followed server-side.
#   4. blocked   — after a block and `helm repo update`, helm's own "not found"
#                  and nothing requested under `charts/`.
#   5. stale     — a block after the client's last `repo update`: the archive
#                  fetch is what refuses it, with a `403` (§4.4).
#   6. cache     — a pull from a fresh client cache is a server cache hit.
#   7. local     — package, `curl --data-binary` to `api/charts`, pull it back
#                  with the same digest; a second POST is `409`; DELETE removes
#                  it from the next index.
#
# Run via `task test:helm-heavy` or directly, optionally with phase names:
#
#     bash tests/heavy/helm.sh            # every phase
#     bash tests/heavy/helm.sh verify     # just that one
#
# Environment knobs: DATABASE_URL (required), HEAVY_PORT (8195),
# HEAVY_TAP_PORT (8196), COVERAGE.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init helm 8195 8196
heavy_need helm "helm (https://helm.sh)"
heavy_need curl "curl"
heavy_need sha256sum "sha256sum"

JET="jet-$HEAVY_RUN"
NGINX="nginx-$HEAVY_RUN"
LOCAL="local-$HEAVY_RUN"
READER="heavy-reader-token"

# Pinned: a chart version is a fact about the upstream on a date, and the
# blocked/stale phases need two neighbours of the pinned one.
CERT_MANAGER="${HELM_CERT_MANAGER:-v1.21.2}"
NGINX_PULL="${HELM_NGINX_PULL:-4.15.1}"
NGINX_BLOCK="${HELM_NGINX_BLOCK:-4.15.0}"
NGINX_STALE="${HELM_NGINX_STALE:-4.14.5}"
# cert-manager's signing keyring, named in its own index entry's
# `artifacthub.io/signKey` annotation. Fetched directly: it is the trust root
# the client verifies against, not something this server should vouch for.
KEYRING_URL="https://cert-manager.io/public-keys/cert-manager-keyring-2021-09-20-1020CF3C033D4F35BAE1C19E1226061C665DF13E.gpg"

heavy_start_server tests/heavy/config.helm.toml
heavy_start_tap
heavy_log "$(helm version --short)"
# `helm pull -d` does not create its destination: the download is written to a
# temp file inside it, which fails with "no such file or directory".
mkdir -p "$HEAVY_WORK"/{verify,absolute,blocked,stale,cachewarm,cache,localpull}

# Every helm state directory inside the run, so the suite never touches the
# runner's repositories and every phase can start from a clean cache.
helm_env() {  # <label> → exports for a fresh client named <label>
  local home="$HEAVY_WORK/helm-$1"
  mkdir -p "$home"
  export HELM_CONFIG_HOME="$home/config" HELM_CACHE_HOME="$home/cache" \
         HELM_DATA_HOME="$home/data" HELM_REPOSITORY_CONFIG="$home/config/repositories.yaml" \
         HELM_REPOSITORY_CACHE="$home/cache/repository"
}

# The client's egress, denied: a dead proxy for everything but loopback, so a
# chart URL left pointing upstream fails instead of quietly reaching GitHub.
DENY=(env HTTPS_PROXY=http://127.0.0.1:9 HTTP_PROXY=http://127.0.0.1:9
      https_proxy=http://127.0.0.1:9 http_proxy=http://127.0.0.1:9
      NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost)

run() {  # <label> <cmd…> → output in $HEAVY_WORK/<label>.out, status returned
  local label="$1"; shift
  "$@" > "$HEAVY_WORK/$label.out" 2>&1
}

said() {  # <label> <ere> <explanation>
  grep -qiE -- "$2" "$HEAVY_WORK/$1.out" && return 0
  tail -n 40 "$HEAVY_WORK/$1.out" >&2
  heavy_fail "$3 (nothing matching /$2/ in $1.out)"
}

repo_add() {  # <client label> <name> <registry> <token>
  helm_env "$1"
  run "$1-add-$2" helm repo add "$2" "$HEAVY_TAP_BASE/proxy/$3/helm" \
    --username ci --password "$4" \
    || { cat "$HEAVY_WORK/$1-add-$2.out" >&2; heavy_fail "helm repo add $2 failed"; }
}

# The served index as this server renders it, for a registry, as the reader.
served_index() {  # <registry>
  curl -fsS -u "ci:$READER" "$HEAVY_BASE/proxy/$1/helm/index.yaml"
}

# Wait until a block has reached the registry-wide index — it is filtered
# against a 30-second snapshot of the blocked set (RFC 0029 §4.4).
await_absent() {  # <registry> <chart> <version>
  local i
  for i in $(seq 1 25); do
    # Not `served_index | grep -q`: grep stops at the first match, curl fails
    # writing the rest, and under pipefail that reads as "absent".
    local index; index="$(served_index "$1")"
    grep -qE "^    version: \"?$3\"?$" <<< "$index" || return 0
    sleep 2
  done
  heavy_fail "$2 $3 was still in the served index 50 s after the block"
}

re() { printf '%s' "$1" | sed 's/[.]/[.]/g'; }

phase_repo_add() {
  heavy_log "1. helm repo add reads the index with basic auth; every URL is on this host"
  heavy_mark repo-add
  repo_add main jet "$JET" "$READER"
  repo_add main nginx "$NGINX" "$READER"
  heavy_wire_re_after repo-add "GET /proxy/$JET/helm/index[.]yaml -> 200" \
    "the jetstack index did not come through the proxy"
  heavy_wire_re_after repo-add "GET /proxy/$NGINX/helm/index[.]yaml -> 200" \
    "the ingress-nginx index did not come through the proxy"

  # What the client cached is what it resolves from.
  local cached
  for cached in "$HELM_REPOSITORY_CACHE/jet-index.yaml" "$HELM_REPOSITORY_CACHE/nginx-index.yaml"; do
    [[ -s "$cached" ]] || heavy_fail "helm cached no index at $cached"
    local foreign
    # Only the `urls:` lists: `keywords:` and `sources:` are lists too.
    foreign="$(awk '/^    urls:/ { u = 1; next } u && /^    - / { print; next } { u = 0 }' "$cached" \
      | grep -vF -- "- $HEAVY_TAP_BASE/proxy/" | head -3 || true)"
    [[ -n "$(awk '/^    urls:/ { u = 1; next } u && /^    - / { print; exit } { u = 0 }' "$cached")" ]] \
      || heavy_fail "found no urls: list in $cached — the check above would pass on nothing"
    [[ -z "$foreign" ]] || heavy_fail "the served index still names URLs off this host: $foreign"
    grep -q 'oci://' "$cached" && heavy_fail "an oci:// entry survived under the default oci_entries"
  done
  return 0
}

phase_verify() {
  heavy_log "2. helm pull --verify fetches the archive and its .prov here, both byte-exact"
  curl -fsSL --proto "=https" --proto-redir "=https" -o "$HEAVY_WORK/cert-manager-keyring.gpg" "$KEYRING_URL" \
    || heavy_fail "could not fetch cert-manager's keyring"
  repo_add verify jet "$JET" "$READER"
  heavy_mark verify
  run verify "${DENY[@]}" helm pull jet/cert-manager --version "$CERT_MANAGER" \
    --verify --keyring "$HEAVY_WORK/cert-manager-keyring.gpg" -d "$HEAVY_WORK/verify" \
    || { cat "$HEAVY_WORK/verify.out" >&2; heavy_fail "helm pull --verify failed through the proxy"; }
  said verify "Signed by|Using Key|Chart Hash Verified" \
    "helm did not report a verified signature"
  local v; v="$(re "$CERT_MANAGER")"
  heavy_wire_re_after verify "GET /proxy/$JET/helm/charts/cert-manager-$v[.]tgz -> 200" \
    "the archive was not fetched from this instance"
  heavy_wire_re_after verify "GET /proxy/$JET/helm/charts/cert-manager-$v[.]tgz[.]prov -> 200" \
    "the provenance file was not fetched from this instance"
  return 0
}

phase_absolute() {
  heavy_log "3. An absolute GitHub URL: the 302 chain is followed server-side"
  repo_add absolute nginx "$NGINX" "$READER"
  heavy_mark absolute
  run absolute "${DENY[@]}" helm pull nginx/ingress-nginx --version "$NGINX_PULL" \
    -d "$HEAVY_WORK/absolute" \
    || { cat "$HEAVY_WORK/absolute.out" >&2; heavy_fail "pulling an absolute-URL chart failed with egress denied"; }
  heavy_wire_re_after absolute \
    "GET /proxy/$NGINX/helm/charts/ingress-nginx-$(re "$NGINX_PULL")[.]tgz -> 200" \
    "the archive was not fetched from this instance"
  # The digest upstream published is the digest of what was served.
  # Captured first: `awk … exit` stops reading, and under pipefail the curl
  # it leaves writing fails the suite with a (23).
  local want got index
  index="$(served_index "$NGINX")"
  want="$(awk -v v="$NGINX_PULL" '
    /^    digest:/ { d = $2 } /^    version:/ { gsub(/"/, "", $2); if ($2 == v) { print d; exit } }' <<< "$index")"
  got="$(sha256sum "$HEAVY_WORK/absolute/ingress-nginx-$NGINX_PULL.tgz" | cut -d' ' -f1)"
  [[ -n "$want" && "$want" == "$got" ]] \
    || heavy_fail "the archive's sha256 ($got) is not the index digest ($want)"
  return 0
}

phase_blocked() {
  heavy_log "4. A blocked version is helm's own 'not found', with nothing downloaded"
  heavy_block "$NGINX" ingress-nginx "$NGINX_BLOCK"
  await_absent "$NGINX" ingress-nginx "$NGINX_BLOCK"
  repo_add blocked nginx "$NGINX" "$READER"
  heavy_mark blocked
  if run blocked "${DENY[@]}" helm pull nginx/ingress-nginx --version "$NGINX_BLOCK" \
      -d "$HEAVY_WORK/blocked"; then
    heavy_unblock "$NGINX" ingress-nginx "$NGINX_BLOCK"
    heavy_fail "pulling a blocked version succeeded"
  fi
  said blocked "not found|no chart version found" "helm did not fail on its own not-found path"
  local hits
  hits="$(heavy_wire_count_after blocked "/proxy/$NGINX/helm/charts/")"
  [[ "$hits" == 0 ]] || heavy_fail "a blocked version still caused $hits archive requests"
  heavy_unblock "$NGINX" ingress-nginx "$NGINX_BLOCK"
  return 0
}

phase_stale() {
  heavy_log "5. A block after the client's last repo update is refused at the archive"
  repo_add stale nginx "$NGINX" "$READER"
  heavy_block "$NGINX" ingress-nginx "$NGINX_STALE"
  heavy_mark stale
  if run stale "${DENY[@]}" helm pull nginx/ingress-nginx --version "$NGINX_STALE" \
      -d "$HEAVY_WORK/stale"; then
    heavy_unblock "$NGINX" ingress-nginx "$NGINX_STALE"
    heavy_fail "pulling a version blocked after the client's index was cached succeeded"
  fi
  heavy_wire_re_after stale \
    "GET /proxy/$NGINX/helm/charts/ingress-nginx-$(re "$NGINX_STALE")[.]tgz -> 403" \
    "the stale-index pull was not refused at the archive with a 403"
  heavy_unblock "$NGINX" ingress-nginx "$NGINX_STALE"
  return 0
}

hits_total() {
  curl -fsS "$HEAVY_BASE/metrics" \
    | awk '/^batlehub_artifact_cache_hits_total/ { n += $2 } END { print n + 0 }'
}

phase_cache() {
  heavy_log "6. A pull from a fresh client cache is a server cache hit"
  repo_add cachewarm nginx "$NGINX" "$READER"
  run cachewarm helm pull nginx/ingress-nginx --version "$NGINX_PULL" -d "$HEAVY_WORK/cachewarm" \
    || heavy_fail "the warming pull failed"
  local before after
  before="$(hits_total)"
  repo_add cache nginx "$NGINX" "$READER"
  run cache helm pull nginx/ingress-nginx --version "$NGINX_PULL" -d "$HEAVY_WORK/cache" \
    || heavy_fail "the second pull failed"
  after="$(hits_total)"
  [[ "$after" -gt "$before" ]] \
    || heavy_fail "batlehub_artifact_cache_hits_total did not move ($before → $after)"
  return 0
}

phase_local() {
  heavy_log "7. ChartMuseum's API: publish, pull back, conflict, delete"
  local src="$HEAVY_WORK/charts"
  mkdir -p "$src"
  ( cd "$src" && helm create heavychart >/dev/null && helm package heavychart >/dev/null ) \
    || heavy_fail "helm create/package failed"
  local tgz="$src/heavychart-0.1.0.tgz"
  [[ -f "$tgz" ]] || heavy_fail "helm package wrote no $tgz"

  heavy_mark publish
  local code
  code="$(curl -sS -o "$HEAVY_WORK/publish.json" -w '%{http_code}' --data-binary "@$tgz" \
    -H "Authorization: Bearer $ADMIN_TOKEN" "$HEAVY_TAP_BASE/proxy/$LOCAL/helm/api/charts")"
  [[ "$code" == 201 ]] || { cat "$HEAVY_WORK/publish.json" >&2; heavy_fail "publish answered $code"; }

  repo_add local mine "$LOCAL" "$READER"
  run localpull "${DENY[@]}" helm pull mine/heavychart --version 0.1.0 -d "$HEAVY_WORK/localpull" \
    || { cat "$HEAVY_WORK/localpull.out" >&2; heavy_fail "pulling the published chart back failed"; }
  [[ "$(sha256sum < "$tgz")" == "$(sha256sum < "$HEAVY_WORK/localpull/heavychart-0.1.0.tgz")" ]] \
    || heavy_fail "the chart pulled back is not the chart published"
  run localsearch helm search repo mine/heavychart
  said localsearch "mine/heavychart" "helm search repo did not list the published chart"

  code="$(curl -sS -o /dev/null -w '%{http_code}' --data-binary "@$tgz" \
    -H "Authorization: Bearer $ADMIN_TOKEN" "$HEAVY_TAP_BASE/proxy/$LOCAL/helm/api/charts")"
  [[ "$code" == 409 ]] || heavy_fail "a second publish of the same version answered $code, not 409"

  code="$(curl -sS -o /dev/null -w '%{http_code}' -X DELETE \
    -H "Authorization: Bearer $ADMIN_TOKEN" "$HEAVY_TAP_BASE/proxy/$LOCAL/helm/api/charts/heavychart/0.1.0")"
  [[ "$code" == 200 ]] || heavy_fail "DELETE answered $code"
  local index; index="$(served_index "$LOCAL")"
  ! grep -q "heavychart" <<< "$index" || heavy_fail "the deleted chart is still in the index"
  return 0
}

PHASES=(repo_add verify absolute blocked stale cache local)

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

heavy_done "HELM HEAVY SUITE PASSED"
