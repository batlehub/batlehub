#!/usr/bin/env bash
# Heavy Open VSX integration test — `ovsx`, against the real client.
#
# RFC 0009 §7.6 predicted this one and phase 7 did not build it: "the OpenVSX
# publish API is what `ovsx publish` calls; we accept only
# `PUT …/{ext}/{version}/vsix`, which no tool sends." It was named in the RFC,
# listed in a phase, never implemented, and **no test failed** — a planned item
# silently not done is invisible to every mechanism in that document except
# running the client (§12.10).
#
# What this proves:
#
#   1. `ovsx publish` succeeds — `POST /api/-/publish?token=…`, with the token
#      in a **query parameter**, which is the only place ovsx puts it. The auth
#      middleware reads headers; the normalisation that accepts `?token=` is
#      scoped to this one route (§13.23), and this is what exercises it.
#   2. It does not print `v1.0.0@undefined`. ovsx appends `@{targetPlatform}`
#      for anything that is not `"universal"`, so a response omitting the field
#      makes a successful publish report itself as broken (§12.14). The
#      assertion is on what the client *printed*, because that was the whole
#      defect: the status code was already 201.
#   3. `ovsx get` downloads the extension back — two requests, the second one
#      reached by following the `files.download` URL out of the first. That
#      rewrite pointing at this proxy is load-bearing: unrewritten, the client
#      goes to open-vsx.org for the bytes, which is the hole phase 5 closed for
#      Terraform.
#
# Run via `task test:openvsx-heavy` or directly. Needs network for `npx` to
# fetch `ovsx` and `@vscode/vsce`.
#
# Environment knobs: DATABASE_URL (required), HEAVY_PORT (8084),
# HEAVY_TAP_PORT (8094), COVERAGE, OVSX_VERSION (1.1.1), VSCE_VERSION (3.9.2).

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init openvsx 8084 8094
heavy_need npx "nodejs"

OVSX_VERSION="${OVSX_VERSION:-1.1.1}"
VSCE_VERSION="${VSCE_VERSION:-3.9.2}"

REGISTRY="vsx-local-$HEAVY_RUN"
# Open VSX ids are `{namespace}.{name}`, and both halves must be valid npm-ish
# identifiers: lowercase, no underscores.
NAMESPACE="heavyorg"
EXT_NAME="probe$HEAVY_RUN"
EXT_ID="$NAMESPACE.$EXT_NAME"
EXT_VERSION="1.0.0"

heavy_start_server tests/heavy/config.openvsx.toml
heavy_start_tap

REGISTRY_URL="$HEAVY_TAP_BASE/proxy/$REGISTRY"

# ── 1. Build a real VSIX ─────────────────────────────────────────────────────
#
# With `vsce`, not with a hand-rolled zip: ovsx reads the package through
# `@vscode/vsce`'s own reader, so a zip that merely looks right is a test of
# our idea of the format rather than of the format.

EXT_DIR="$HEAVY_WORK/extension"
mkdir -p "$EXT_DIR"
cat > "$EXT_DIR/package.json" <<EOF
{
  "name": "$EXT_NAME",
  "displayName": "RFC 0009 heavy probe",
  "description": "Published to a BatleHub instance and fetched back.",
  "publisher": "$NAMESPACE",
  "version": "$EXT_VERSION",
  "license": "MIT",
  "engines": { "vscode": "^1.75.0" },
  "categories": ["Other"],
  "main": "./extension.js",
  "activationEvents": ["onStartupFinished"],
  "contributes": {}
}
EOF
cat > "$EXT_DIR/extension.js" <<'EOF'
function activate() {}
module.exports = { activate };
EOF
echo "# RFC 0009 heavy probe" > "$EXT_DIR/README.md"
echo "MIT" > "$EXT_DIR/LICENSE"

VSIX="$HEAVY_WORK/$EXT_NAME-$EXT_VERSION.vsix"
heavy_log "Packaging the VSIX with vsce $VSCE_VERSION"
(cd "$EXT_DIR" && npx --yes "@vscode/vsce@$VSCE_VERSION" package \
  --no-dependencies --allow-missing-repository --skip-license -o "$VSIX") \
  >"$HEAVY_WORK/vsce.log" 2>&1 \
  || { tail -30 "$HEAVY_WORK/vsce.log" >&2; heavy_fail "vsce package failed"; }
[[ -s "$VSIX" ]] || heavy_fail "vsce produced no VSIX"

# ── 2. ovsx publish ──────────────────────────────────────────────────────────

heavy_mark "publish"
heavy_log "ovsx publish -> $REGISTRY_URL"
set +e
npx --yes "ovsx@$OVSX_VERSION" publish "$VSIX" \
  --registryUrl "$REGISTRY_URL" --pat "$ADMIN_TOKEN" \
  >"$HEAVY_WORK/publish.log" 2>&1
PUBLISH_RC=$?
set -e
cat "$HEAVY_WORK/publish.log"
[[ $PUBLISH_RC -eq 0 ]] || heavy_fail "ovsx publish failed (rc=$PUBLISH_RC)"

heavy_wire_after "publish" "POST /proxy/$REGISTRY/api/-/publish" \
  "ovsx did not reach the publish endpoint"
grep -q "POST /proxy/$REGISTRY/api/-/publish.*token=" "$HEAVY_LOG" \
  || heavy_fail "the publish request carried no ?token= — the credential ovsx sends"
grep -q "POST /proxy/$REGISTRY/api/-/publish.* -> 20" "$HEAVY_LOG" \
  || heavy_fail "the publish endpoint did not answer 2xx"

# What the client printed, not what the server returned. `@undefined` is a
# successful publish reporting itself as broken (§12.14).
grep -q "@undefined" "$HEAVY_WORK/publish.log" \
  && heavy_fail "ovsx printed '@undefined' — the response omits targetPlatform"
grep -q "$EXT_ID" "$HEAVY_WORK/publish.log" \
  || heavy_fail "ovsx did not report publishing $EXT_ID"
heavy_log "OVSX-PUBLISH-OK ($EXT_ID published with a query-parameter token)"

# ── 3. ovsx get — metadata, then the file link it names ──────────────────────

heavy_mark "get"
heavy_log "ovsx get $EXT_ID"
(cd "$HEAVY_WORK" && npx --yes "ovsx@$OVSX_VERSION" get "$EXT_ID" \
  --registryUrl "$REGISTRY_URL" -o "$HEAVY_WORK/downloaded.vsix") \
  >"$HEAVY_WORK/get.log" 2>&1 \
  || { cat "$HEAVY_WORK/get.log" >&2; heavy_fail "ovsx get failed"; }

[[ -s "$HEAVY_WORK/downloaded.vsix" ]] || heavy_fail "ovsx get wrote no file"
head -c 2 "$HEAVY_WORK/downloaded.vsix" | grep -q "PK" \
  || heavy_fail "what came back is not a ZIP"
cmp -s "$VSIX" "$HEAVY_WORK/downloaded.vsix" \
  || heavy_fail "the downloaded VSIX differs from the published one"

heavy_wire_after "get" "GET /proxy/$REGISTRY/api/$NAMESPACE/$EXT_NAME -> 200" \
  "ovsx get did not read the extension document"
# The file URL came out of that document. Reaching us means `files.download`
# was rewritten; unrewritten it would have gone to open-vsx.org, where this
# extension does not exist — and the download would have 404'd there rather
# than here, which is the failure that looks like someone else's outage.
#
# The assertion is "a second request, to this proxy, that returned the bytes",
# not a fixed path. It is `…/api/{ns}/{ext}/{ver}/file/{ns}.{ext}-{ver}.vsix`
# today — the shape RFC 0009 §12.6 recorded against open-vsx.org, and the one
# `ovsx` takes its output filename from — but it was the VS Code gallery asset
# route (`/vscode/asset/{ns}/{ext}/{ver}/…VSIXPackage`) until the renderer was
# corrected. ovsx follows whatever the document says, so pinning one shape here
# would make a legal change to the renderer fail this test for no reason; the
# closed-world suite's ovsx phase is where the exact route is asserted.
awk -v mark="### get" -v reg="/proxy/$REGISTRY/" '
    index($0, mark) == 1 { seen = 1; next }
    seen && index($0, reg) && /-> 200/ && !/\/api\/[^\/]+\/[^\/]+ ->/ { found = 1 }
    END { exit found ? 0 : 1 }' "$HEAVY_LOG" \
  || heavy_fail "the VSIX was not fetched through the proxy — files.download may point upstream"
heavy_log "OVSX-GET-OK (metadata, then the file link it named)"

heavy_done OPENVSX-HEAVY-OK
