#!/usr/bin/env bash
# Manual canary for RFC 0035 §11 q3 — **Eclipse Che's own dashboard backend**
# reading a `devfile` registry, rather than a transcription of it.
#
# `tests/heavy/devfile.sh` case 9 replays Che's reads with `curl`. This drives
# the real code: the dashboard backend's `POST /dashboard/api/data/resolver`
# (`routes/api/dataResolver.ts`), which is how the *Get Started* page fetches
# an external registry's index and each tile's devfile — server-side, with no
# credential, no redirect, and a refusal for a private IP literal. It is not a
# CI job: it needs a Che installation, a token that can call its dashboard
# API, and a server this cluster can reach. Run it from a Che workspace.
#
# Read-only against Che: nothing is configured there. The *Get Started* tiles
# themselves need the registry in the `CheCluster`'s
# `devfileRegistry.externalDevfileRegistries`, a cluster change this script
# does not make; what the tiles render is `resolveLinks` over the document
# the resolver returns, which case 2 recomputes the way the dashboard does.
#
#   1  the resolver reads `index/all` through the path prefix and every stack
#      entry passes `isDevfileMetaData` (displayName, icon, links, tags — an
#      entry missing one is dropped from the page);
#   2  the tile link `resolveLinks` builds from `links.self` answers the same
#      devfile a direct read does;
#   3  after a block of `nodejs`'s default, `index/all` no longer names
#      `nodejs` and the blocked version's tile link is refused;
#   4  the same index named by the pod's IP address is refused by Che itself
#      (`Requests to private addresses are not allowed`) — why the docs say
#      to give Che a hostname.
#
# Environment: DATABASE_URL (required); CHE_DASHBOARD_API (default the
# in-cluster dashboard service); CHE_TOKEN (default the current kubeconfig
# user's token); CANARY_BASE (default this workspace's Service on 8080).

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init devfile_che_canary 8080 8079
heavy_need node "node (resolveLinks, recomputed)"
heavy_need python3 "python3"

API="${CHE_DASHBOARD_API:-http://che-dashboard.eclipse-che.svc:8080}"
TOKEN="${CHE_TOKEN:-$(kubectl config view --raw --minify -o jsonpath='{.users[0].user.token}' 2>/dev/null)}"
[[ -n "$TOKEN" ]] || heavy_fail "no CHE_TOKEN and no token in the current kubeconfig"
: "${DEVWORKSPACE_ID:?not in a Che workspace — set CANARY_BASE to a URL the Che dashboard can reach}"
BASE="${CANARY_BASE:-http://${DEVWORKSPACE_ID}-service.${DEVWORKSPACE_NAMESPACE}.svc:8080}"
REG="devfile-$HEAVY_RUN"
ROOT="$BASE/proxy/$REG/"

heavy_start_server tests/heavy/config.devfile-che-canary.toml

# resolve <url> <out> — Che's resolver fetching <url>; echoes its status.
resolve() {
  local url="$1" out="$2"
  curl -s --max-time 60 -o "$out" -w '%{http_code}' -X POST "$API/dashboard/api/data/resolver" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    --data "$(python3 -c 'import json,sys; print(json.dumps({"url": sys.argv[1]}))' "$url")"
  return $?
}

# tile_link <root> <links.self> — what `resolveLinks` makes of an entry.
tile_link() {
  local root="$1" self="$2"
  node -e '
const [root, self] = process.argv.slice(1);
const catalog = new URL("devfiles", root).toString();
console.log(self.startsWith("devfile-catalog") ? self.replace(":", "/").replace("devfile-catalog", catalog) : self);
' "$root" "$self"
  return $?
}

# ── 1 ────────────────────────────────────────────────────────────────────────
code="$(resolve "${ROOT}index/all" "$HEAVY_WORK/index.json")"
[[ "$code" == 200 ]] || { cat "$HEAVY_WORK/index.json" >&2; heavy_fail "1: Che's resolver answered $code for ${ROOT}index/all"; }
python3 - "$HEAVY_WORK/index.json" <<'PY' || heavy_fail "1: an index entry Che would drop"
import json, sys
index = json.load(open(sys.argv[1]))
stacks = [e for e in index if e.get("type") == "stack"]
assert stacks, "no stacks"
bad = [e["name"] for e in stacks if any(e.get(k) is None for k in ("displayName", "icon", "links", "tags"))]
assert not bad, f"isDevfileMetaData would drop: {bad}"
assert any(e["name"] == "nodejs" for e in stacks)
print(f"1: {len(stacks)} stacks, each a tile Che keeps")
PY

# ── 2 ────────────────────────────────────────────────────────────────────────
NODE_SELF="$(python3 -c 'import json,sys; print([e for e in json.load(open(sys.argv[1])) if e["name"]=="nodejs"][0]["links"]["self"])' "$HEAVY_WORK/index.json")"
NODE_DEFAULT="${NODE_SELF##*:}"
LINK="$(tile_link "$ROOT" "$NODE_SELF")"
code="$(resolve "$LINK" "$HEAVY_WORK/tile.yaml")"
[[ "$code" == 200 ]] || { cat "$HEAVY_WORK/tile.yaml" >&2; heavy_fail "2: the tile link $LINK answered $code through Che"; }
curl -fsS "$HEAVY_BASE/proxy/$REG/devfiles/nodejs/$NODE_DEFAULT" -o "$HEAVY_WORK/direct.yaml"
# The resolver hands back `response.data`: axios parsed nothing (text/plain),
# so the body is the YAML — as a JSON string when Fastify serialised it.
python3 - "$HEAVY_WORK/tile.yaml" "$HEAVY_WORK/direct.yaml" <<'PY' || heavy_fail "2: Che's devfile is not the one the registry serves"
import json, sys
got = open(sys.argv[1]).read()
try:
    got = json.loads(got) if got.startswith('"') else got
except ValueError:
    pass
assert got == open(sys.argv[2]).read(), (got[:200],)
PY
heavy_log "2: the tile link $LINK is nodejs@$NODE_DEFAULT's devfile, through Che"

# ── 3 ────────────────────────────────────────────────────────────────────────
heavy_block "$REG" nodejs "$NODE_DEFAULT"
sleep 31
code="$(resolve "${ROOT}index/all" "$HEAVY_WORK/index-blocked.json")"
[[ "$code" == 200 ]] || heavy_fail "3: the index answered $code after the block"
python3 -c 'import json,sys; assert "nodejs" not in [e["name"] for e in json.load(open(sys.argv[1]))]' \
  "$HEAVY_WORK/index-blocked.json" || heavy_fail "3: nodejs is still a tile after its default was blocked"
code="$(resolve "$LINK" "$HEAVY_WORK/tile-blocked.txt")"
[[ "$code" != 200 ]] || heavy_fail "3: the blocked version's tile link still answered 200"
heavy_log "3: after blocking nodejs@$NODE_DEFAULT, Che's index has no nodejs tile and the old link answers $code"

# ── 4 ────────────────────────────────────────────────────────────────────────
# The IPv4 address: `hostname -i` may list an IPv6 one first, which is not a
# URL host without brackets, and Che's check is written against 10/8 and kin.
POD_IP="$(hostname -I | tr ' ' '\n' | grep -m1 -E '^[0-9]+([.][0-9]+){3}$')"
code="$(resolve "http://$POD_IP:8080/proxy/$REG/index/all" "$HEAVY_WORK/private.txt")"
[[ "$code" == 403 ]] && grep -q "private addresses" "$HEAVY_WORK/private.txt" \
  || { cat "$HEAVY_WORK/private.txt" >&2; heavy_fail "4: Che answered $code for a private IP literal, not its own 403"; }
heavy_log "4: Che refuses http://$POD_IP:8080/… itself: $(cat "$HEAVY_WORK/private.txt")"

heavy_done "devfile Che canary passed: index read by Che's resolver, every stack a kept tile, tile link resolved, a block observed, private IP refused"
