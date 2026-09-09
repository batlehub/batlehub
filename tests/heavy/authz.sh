#!/usr/bin/env bash
# Heavy authorization test — RFC 0015's grants, against real clients and a real
# server.
#
# Every other heavy suite asks whether a client can do the thing. This one asks
# whether a client that **may not** is stopped, and whether the one that may is
# not stopped by accident. Both halves are required, and the second is the one
# that is usually missing: §13.16 records all three new ecosystem verbs shipping
# *unreachable* — held by nobody, so every endpoint answered `403` to the
# administrator — while every test passed, because each test granted its own
# verb explicitly and nothing ever asserted the identical request works for
# somebody. A negative test cannot distinguish a correct denial from a denial
# for the wrong reason (§13.17). So every assertion here is a pair.
#
# ── What only this layer can prove ───────────────────────────────────────────
#
# `crates/web/tests/authz_matrix.rs` already walks the route table in-process,
# and it is the more precise instrument. Three things it structurally cannot
# see, all of which have shipped broken before:
#
#   1. **The TOML.** The matrix builds its hierarchy from fixtures in Rust. The
#      path from `[registries.grants]` in a file, through the loader, through
#      §10's translation, into the resolver, is exercised by nothing that boots
#      a server — and §13.18 found `config.example.toml` offering a verb that
#      has never existed and stops the server starting. This suite's grants come
#      from a file the loader reads.
#   2. **The client.** A `403` a package manager turns into a silent partial
#      install is not a boundary. RFC 0009 §5.2's whole point is that "the route
#      answers correctly" and "the client is stopped" are different claims, and
#      only one of them can be checked with `curl`.
#   3. **The diagnostic agreeing with the server.** §11.6: *"a diagnostic that
#      can disagree with reality is worse than none, because it is trusted."*
#      It has disagreed twice — under a shadow (§13.7) and at the instance tier
#      (§13.9), where `explain` said `deny` and the server said `allow`. Here
#      `explain` is asked about the same request the wire just answered, on the
#      same running server, and the two must agree.
#
# ── The vocabulary is covered, and the suite proves that it is ───────────────
#
# The verb list is read out of `crates/core/src/entities/permission.rs` at run
# time rather than copied here, and the run fails if it ends with a verb it
# never exercised. A suite that silently stops covering a verb the day one is
# added is the failure mode §11.5's dead-end test exists to prevent, one layer
# up; this is the same check asked of the wire instead of of the source.
#
# ── Usage ────────────────────────────────────────────────────────────────────
#
#     tests/heavy/authz.sh [target]
#
#   matrix      every verb, every grant shape, over curl        (no client)
#   signing     RFC 0012 capabilities: artifact binding, expiry, and secret
#               rotation in both directions                      (no client)
#   npm | pypi | nuget | composer | conda | openvsx | rubygems | terraform
#               the pull boundary, driven by that ecosystem's real client
#
# One target per invocation, like every other heavy suite: each starts its own
# server and its own tap, and Terraform's tap has to terminate TLS while the
# others must not. `task test:authz-heavy` runs them in sequence; CI runs them
# as a matrix.
#
# Environment knobs: DATABASE_URL (required), HEAVY_PORT (8089), HEAVY_TAP_PORT
# (8099, or 8444 for terraform), COVERAGE, plus the per-client version pins the
# corresponding ecosystem suite documents.

TARGET="${1:-matrix}"

# Terraform refuses a plain-`http:` registry host outright (RFC 0009 §12.3), so
# its tap terminates TLS on a port that looks like one. Set before `heavy_init`,
# which is what reads the default.
if [[ "$TARGET" == "terraform" ]]; then
  HEAVY_TAP_PORT="${HEAVY_TAP_PORT:-8444}"
  export HEAVY_TAP_HOST=localhost
fi

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init authz 8089 8099
heavy_need python3 "python3"
heavy_need curl "curl"

# `until` is required on a shadow block and config load refuses a date already
# past — a shadow that cannot be forgotten is the entire point (§4.7) — so the
# date cannot be committed. Thirty days out: long enough that a clock skew or a
# slow CI queue cannot expire it mid-run, short enough to be obviously a test.
HEAVY_SHADOW_UNTIL="$(date -u -d '+30 days' +%F 2>/dev/null || date -u -v+30d +%F)"
export HEAVY_SHADOW_UNTIL

# RFC 0012's instance signing key. Interpolated by the loader rather than
# committed, and 32 bytes is the floor it enforces. A fixed value rather than a
# random one: the run must be reproducible, and this secret protects nothing but
# a throwaway registry on 127.0.0.1.
export HEAVY_SIGNING_SECRET="heavy-authz-url-signing-secret-not-a-real-key-0123456789"

# ── The roster ───────────────────────────────────────────────────────────────

NPM="authz-npm-$HEAVY_RUN"
SHADOW="authz-shadow-$HEAVY_RUN"
PYPI="authz-pypi-$HEAVY_RUN"
NUGET="authz-nuget-$HEAVY_RUN"
COMPOSER_REG="authz-composer-$HEAVY_RUN"
CONDA="authz-conda-$HEAVY_RUN"
VSX="authz-vsx-$HEAVY_RUN"
GEMS="authz-gems-$HEAVY_RUN"
TFREG="authz-tf-$HEAVY_RUN"
JB="authz-jb-$HEAVY_RUN"

T_ADMIN="$ADMIN_TOKEN"
T_READER="authz-reader-token"
T_DENIED="authz-denied-token"
T_LISTER="authz-lister-token"
T_BROWSER="authz-browser-token"
T_SRE="authz-sre-token"
T_AUDITOR="authz-auditor-token"
T_GRANTS_READER="authz-grants-reader-token"
T_SUPPORT="authz-support-token"
T_GATE="authz-gatekeeper-token"
# The literal used where a request must carry no credential at all. `-` rather
# than an empty string so an accidentally-unset token variable cannot silently
# turn an authenticated assertion into an anonymous one that still passes.
T_ANON="-"

PKG="authz-probe-$HEAVY_RUN"
# The subject RFC 0017's version-tier grant is written for. Holds nothing in
# `config.authz.toml` and is named nowhere else, so an `allow` about it is
# attributable to the row the suite writes and to no config tier.
GRANTEE="authz-grantee-$HEAVY_RUN"
TEAM_PKG="@team/probe-$HEAVY_RUN"
# The segment-boundary probe. `@teamx` shares `@team`'s prefix and is a
# different namespace; RFC 0011-bis §4.2 records the bug where it was not
# (`digital` matching `digital.pipeline-tools`).
TEAMX_PKG="@teamx/probe-$HEAVY_RUN"
SEALED_PKG="@sealed/probe-$HEAVY_RUN"
# A scope whose namespace grants the metadata verbs and not `source:read`.
META_PKG="@metaonly/probe-$HEAVY_RUN"
# On the shadowed registry, under the namespace whose grants are in shadow.
SHADOW_PKG="@shadowed/probe-$HEAVY_RUN"

# ── The literals this suite repeats ──────────────────────────────────────────
#
# Named rather than retyped, because each of them is load-bearing in a way a
# typo does not announce. A JSON body sent without the header is a `400` from an
# extractor that runs *before* the handler, so the request never reaches the gate
# and a negative arm passes without authorization having been consulted (the
# reason the body is not optional at §13's `explore/invalidate`). A wire
# assertion spelled `-> 403` in twenty places and `->403` in one is an assertion
# that silently stops matching. And a label is what `heavy_fail` prints: the same
# subject has to read the same way in every line of the report.
HDR_JSON="Content-Type: application/json"
WIRE_403="-> 403"
IP_BLOCKS="/api/v1/admin/ip-blocks"
WHO_DENIED="the denied user"

# ── Assertion helpers ────────────────────────────────────────────────────────

AUTHZ_CHECKS=0
# Verbs this run has exercised, checked against the enum at the end.
declare -A AUTHZ_VERB_SEEN=()

authz_note_verb() {
  local verb="$1"
  AUTHZ_VERB_SEEN["$verb"]=1
  return 0
}

# authz_status <method> <token|-> <path> [curl args…] — echoes the status code.
#
# Through the tap, not through the server: the transcript is what `heavy_fail`
# dumps, and an assertion whose request never appears in it is unactionable.
authz_status() {
  local method="$1" token="$2" path="$3"
  shift 3
  local args=(-s -o /dev/null -w '%{http_code}' -X "$method")
  if [[ "$token" != "-" ]]; then
    args+=(-H "Authorization: Bearer $token")
  fi
  curl "${args[@]}" "$@" "$HEAVY_TAP_BASE$path"
  return 0
}

# authz_body <method> <token|-> <path> [curl args…] — echoes the response body.
authz_body() {
  local method="$1" token="$2" path="$3"
  shift 3
  local args=(-s -X "$method")
  if [[ "$token" != "-" ]]; then
    args+=(-H "Authorization: Bearer $token")
  fi
  curl "${args[@]}" "$@" "$HEAVY_TAP_BASE$path"
  return 0
}

# authz_denied <verb> <label> <method> <token> <path> [curl args…]
#
# **Exactly `403`.** Not "some error": a `404` where a `403` was meant is how
# §4.4 rule 2's deliberate not-found and an accidental routing failure become
# indistinguishable, and a `400` means the request never reached the gate.
authz_denied() {
  local verb="$1" label="$2"
  shift 2
  # What is left is `<method> <token> <path> [curl args…]`, and the report names
  # the request rather than the argument position it came from.
  local method="$1" path="$3"
  authz_note_verb "$verb"
  local got
  got="$(authz_status "$@")"
  if [[ "$got" != "403" ]]; then
    heavy_fail "$verb — $label: expected 403, got $got ($method $path)"
  fi
  AUTHZ_CHECKS=$((AUTHZ_CHECKS + 1))
  return 0
}

# authz_allowed <verb> <label> <method> <token> <path> [curl args…]
#
# **Not-403, rather than a specific success.** The claim being made is
# "authorization did not refuse this", and a holder can legitimately meet a
# `404` (no such package), a `400` (a body this call did not bother to build) or
# a `409` (already exists) on the far side of the gate. Asserting `200` would
# make every positive control a hostage to the fixture, and the fixtures are not
# what is under test. A `401` is treated as a refusal too: it is the
# authentication layer refusing, which is still "this request did not get past
# the gates", and a positive control that passes on one is not a control.
authz_allowed() {
  local verb="$1" label="$2"
  shift 2
  local method="$1" path="$3"
  authz_note_verb "$verb"
  local got
  got="$(authz_status "$@")"
  if [[ "$got" == "403" || "$got" == "401" ]]; then
    heavy_fail "$verb — $label: expected anything but a refusal, got $got ($method $path). \
This is the positive control: the verb is requested by a route nobody can reach."
  fi
  AUTHZ_CHECKS=$((AUTHZ_CHECKS + 1))
  return 0
}

# authz_refused_anonymous <verb> <label> <method> <path> [curl args…]
#
# `401` or `403`. Which one a route answers an unauthenticated caller depends on
# whether the auth layer or the grant layer speaks first, and that is a property
# of the middleware chain rather than of this document — pinning either spelling
# here would be asserting something this suite has no opinion about.
authz_refused_anonymous() {
  local verb="$1" label="$2" method="$3" path="$4"
  shift 4
  authz_note_verb "$verb"
  local got
  got="$(authz_status "$method" "$T_ANON" "$path" "$@")"
  if [[ "$got" != "403" && "$got" != "401" ]]; then
    heavy_fail "$verb — $label: an unauthenticated caller got $got, expected 401 or 403 ($method $path)"
  fi
  AUTHZ_CHECKS=$((AUTHZ_CHECKS + 1))
  return 0
}

# authz_explain <registry> <subject> <action> [package] [version]
#   — echoes allow | deny.
#
# The oracle. `explain` resolves the same hierarchy the request path does, and
# §11.6 requires the two to agree; asking it here, on the running server, is the
# only place that claim is checked against an answer the server actually gave.
#
# `version` is the fifth argument because RFC 0017 made the deepest tier
# writable, and `explain` was resolving without it: it composed the tiers a
# config file declares and stopped, while the request path appends the stored
# package and version nodes afterwards. So it answered `deny` where the server
# answers `allow` — the third time this diagnostic has drifted from the server
# (§13.7 under a shadow, §13.9 at the instance tier), and the reason a version
# argument that nothing passed would be a coverage hole rather than an
# unimplemented convenience.
authz_explain() {
  local registry="$1" subject="$2" action="$3" package="${4:-}" version="${5:-}"
  local q
  # Encoded by a real encoder rather than pasted: a `*` subject and the `:` in
  # `group:*:eng` are not URL-safe, and a hand-built query answers about a
  # different subject while looking correct.
  q="$(python3 -c '
import sys, urllib.parse
registry, subject, action, package, version = sys.argv[1:6]
fields = {"registry": registry, "subject": subject, "action": action}
if package:
    fields["package"] = package
if version:
    fields["version"] = version
print(urllib.parse.urlencode(fields))
' "$registry" "$subject" "$action" "$package" "$version")"
  authz_body GET "$T_ADMIN" "/api/v1/admin/authz/explain?$q" | python3 -c '
import json, sys
try:
    print(json.load(sys.stdin).get("decision", "?"))
except Exception:
    print("?")
'
  return 0
}

# authz_oracle <verb> <registry> <subject> <expected> [package] [version]
#
# Asserts `explain` answers what the wire just did.
authz_oracle() {
  local verb="$1" registry="$2" subject="$3" expected="$4" package="${5:-}" version="${6:-}"
  local got
  got="$(authz_explain "$registry" "$subject" "$verb" "$package" "$version")"
  if [[ "$got" != "$expected" ]]; then
    heavy_fail "explain disagrees with the server: $subject / $verb on \
${package:-$registry}${version:+@$version} — the wire said $expected, explain says '$got'. \
A diagnostic that can disagree with reality is worse than none (§11.6)."
  fi
  AUTHZ_CHECKS=$((AUTHZ_CHECKS + 1))
  return 0
}

# ── Fixtures ─────────────────────────────────────────────────────────────────

# authz_npm_publish <token> <name> <version> — echoes the status code.
#
# The wire format `npm publish` sends, built here rather than by npm: the
# matrix phase must be able to publish a *scoped* name into a sealed namespace
# and read the refusal, and installing node to do it would make the fast phase
# depend on the slow one's toolchain. The npm phase below uses the real client.
authz_npm_publish() {
  local token="$1" name="$2" version="$3"
  local body="$HEAVY_WORK/npm-publish.json"
  python3 - "$name" "$version" > "$body" <<'PY'
import base64, json, sys
name, version = sys.argv[1], sys.argv[2]
tarball = base64.b64encode(b"authz-heavy-probe").decode()
print(json.dumps({
    "name": name,
    "versions": {version: {
        "name": name,
        "version": version,
        "description": "RFC 0015 heavy authorization probe",
        "dist": {"shasum": "0" * 40},
    }},
    "_attachments": {f"{name.split('/')[-1]}-{version}.tgz": {
        "content_type": "application/octet-stream",
        "data": tarball,
        "length": len(base64.b64decode(tarball)),
    }},
}))
PY
  # The scope separator is percent-encoded because the publish route takes the
  # whole name as one path segment, which is what npm itself sends.
  local encoded="${name//\//%2f}"
  authz_status PUT "$token" "/proxy/$NPM/$encoded" \
    -H "$HDR_JSON" --data-binary @"$body"
  return 0
}

# ═════════════════════════════════════════════════════════════════════════════
# The matrix — every verb, every grant shape
# ═════════════════════════════════════════════════════════════════════════════

phase_matrix() {
  heavy_log "Seeding fixtures as the administrator"

  local rc
  rc="$(authz_npm_publish "$T_ADMIN" "$PKG" 1.0.0)"
  [[ "$rc" == 2* ]] || heavy_fail "seeding $PKG failed with $rc — the administrator cannot publish, so nothing below means anything"
  rc="$(authz_npm_publish "$T_ADMIN" "$TEAM_PKG" 1.0.0)"
  [[ "$rc" == 2* ]] || heavy_fail "seeding $TEAM_PKG failed with $rc"
  rc="$(authz_npm_publish "$T_ADMIN" "$TEAMX_PKG" 1.0.0)"
  [[ "$rc" == 2* ]] || heavy_fail "seeding $TEAMX_PKG failed with $rc"
  rc="$(authz_npm_publish "$T_ADMIN" "$META_PKG" 1.0.0)"
  [[ "$rc" == 2* ]] || heavy_fail "seeding $META_PKG failed with $rc"

  # ── 1. source:read — the pull boundary ─────────────────────────────────────
  #
  # The bytes a package manager downloads, which on npm is `source:read` rather
  # than `releases:read`: a tarball is a source archive, and
  # `handlers/proxy/npm/read.rs` says so at the route. Getting this label right
  # matters more than it looks — the reader holds all three read verbs, so an
  # assertion mislabelled `releases:read` would pass here and would be testing a
  # verb it never named.
  #
  # Same URL, same server, same second; the only thing that differs is which
  # `user:` the token resolves to.

  heavy_mark "source-read"
  heavy_log "source:read — the pull boundary"

  authz_denied source:read "$WHO_DENIED pulls an artifact" \
    GET "$T_DENIED" "/proxy/$NPM/$PKG/1.0.0/tarball"
  authz_allowed source:read "the reader pulls the same artifact" \
    GET "$T_READER" "/proxy/$NPM/$PKG/1.0.0/tarball"
  authz_refused_anonymous source:read "no credential at all" \
    GET "/proxy/$NPM/$PKG/1.0.0/tarball"
  authz_denied source:read "the lister, who holds neither read verb for bytes" \
    GET "$T_LISTER" "/proxy/$NPM/$PKG/1.0.0/tarball"

  authz_oracle source:read "$NPM" "user:authz-denied" deny "$PKG"
  authz_oracle source:read "$NPM" "user:authz-reader" allow "$PKG"

  # ── 2. releases:list — wired on one document path and not the other ────────
  #
  # §4.2 splits `releases:list` out of `releases:read` so that "build agents
  # resolve everything, people browse nothing" is expressible, and §13.14 records
  # the verb being wired at `authorize_listing`.
  #
  # There are **two** document paths, and only one of them reaches that funnel
  # before deciding:
  #
  #   `fetch_proxy_document` → `authorize_listing_audited`, which substitutes
  #   `Action::ReleasesList` for the handler's own action on purpose
  #   (`services/proxy/handle.rs`). The PyPI simple page and Composer's
  #   `packages.json` go this way, and a caller holding `releases:list` alone is
  #   served them — verified by this suite's §15 and by the pypi client phase.
  #
  #   `local_first` (`handlers/proxy/common.rs`) → `authorize_read(pkg, identity,
  #   action)` with the **handler's** action, before the local service is called
  #   at all. Every handler that goes this way passes `Action::ReleasesRead`, so
  #   the `authorize_listing(ReleasesList)` inside the local service runs only
  #   after `releases:read` has already been required. The npm packument on a
  #   local registry is one of these, and the assertions below are it: a subject
  #   granted `releases:list` alone is refused.
  #
  # So the split is real on one path and unreachable on the other, and which one
  # a document takes is not a property an operator writing a grant can see. The
  # suite pins **what the server does** rather than what §4.2 describes —
  # asserting the RFC would leave a red suite describing an intention — and the
  # day `local_first` passes a listing verb, these assertions fail and whoever
  # changed it reads this paragraph.

  heavy_mark "releases-list"
  heavy_log "releases:list — pinned as the server implements it, not as §4.2 describes it"

  authz_denied releases:list "the lister reads an npm packument (the split is not wired here)" \
    GET "$T_LISTER" "/proxy/$NPM/$PKG"
  authz_denied releases:list "$WHO_DENIED reads the packument" \
    GET "$T_DENIED" "/proxy/$NPM/$PKG"
  authz_allowed releases:list "the reader, who holds releases:read as well" \
    GET "$T_READER" "/proxy/$NPM/$PKG"

  # The oracle is asked about `releases:read`, which is the verb this route
  # actually requests. Asking it about `releases:list` would report a
  # disagreement that is the route's, not the resolver's — the model does grant
  # the lister that verb, and `explain` is right to say so.
  authz_oracle releases:read "$NPM" "user:authz-lister" deny "$PKG"

  # ── 3. The namespace tier: widening, and the segment boundary ──────────────
  #
  # The sentence `[registries.rbac]` could not say. One token, one registry,
  # two packages, two answers.

  heavy_mark "namespace"
  heavy_log "the namespace tier — a grant below the registry widens, and stops at the separator"

  local team_enc="${TEAM_PKG//\//%2f}" teamx_enc="${TEAMX_PKG//\//%2f}"
  local meta_enc="${META_PKG//\//%2f}"
  authz_allowed source:read "$WHO_DENIED pulls from the namespace that grants them" \
    GET "$T_DENIED" "/proxy/$NPM/$team_enc/1.0.0/tarball"
  authz_denied source:read "…and is still refused one namespace over" \
    GET "$T_DENIED" "/proxy/$NPM/$teamx_enc/1.0.0/tarball"
  authz_oracle source:read "$NPM" "user:authz-denied" allow "$TEAM_PKG"
  authz_oracle source:read "$NPM" "user:authz-denied" deny "$TEAMX_PKG"

  # A namespace granting the metadata verbs and not `source:read`: the same
  # caller resolves every version of the package and can install none of them.
  # Defensible as a policy, and the shape of an operator's mistake when they read
  # `releases:read` as "may download" — either way it is now observable.
  authz_allowed releases:read "the metadata-only namespace resolves" \
    GET "$T_DENIED" "/proxy/$NPM/$meta_enc"
  authz_denied source:read "…and hands over no bytes" \
    GET "$T_DENIED" "/proxy/$NPM/$meta_enc/1.0.0/tarball"

  # ── 4. The seal, and the administrative floor ──────────────────────────────
  #
  # `grants = {}` is the only construct in the model that takes access away, and
  # the only reason a `role:user` cannot publish to a local registry: §10 rule 5
  # hands `releases:publish` to every `role:user` and grants are additive, so
  # withholding it is not otherwise expressible.

  heavy_mark "seal"
  heavy_log "the seal — it stops inheritance, including the administrator's"

  rc="$(authz_npm_publish "$T_ADMIN" "$SEALED_PKG" 1.0.0)"
  [[ "$rc" == "403" ]] || heavy_fail \
    "the administrator published into a sealed namespace and got $rc — a seal that \
role:admin walks through is not a seal (§4.3)"
  authz_note_verb releases:publish
  AUTHZ_CHECKS=$((AUTHZ_CHECKS + 1))

  rc="$(authz_npm_publish "$T_DENIED" "$SEALED_PKG" 1.0.0)"
  [[ "$rc" == "403" ]] || heavy_fail "a role:user published into a sealed namespace and got $rc"
  AUTHZ_CHECKS=$((AUTHZ_CHECKS + 1))

  # The floor. An administrator can always see what a seal contains and change
  # it — a subtree nobody can reopen is an outage that looks like a config.
  authz_allowed owners:read "the administrative floor survives the seal" \
    GET "$T_ADMIN" "/api/v1/admin/registries/$NPM/packages/${SEALED_PKG//\//%2F}/owners"
  # …and nothing else does: the floor is the ability to administer the sealed
  # node, never to use it.
  authz_denied source:read "the read verbs do not survive the seal" \
    GET "$T_ADMIN" "/proxy/$NPM/${SEALED_PKG//\//%2f}/1.0.0/tarball"

  # ── 5. Grants only widen — the surprise, pinned ────────────────────────────
  #
  # The denied user cannot read a byte of this registry and can publish to it,
  # because publish authority comes from §10 rule 5's `role:user` grant and no
  # deeper node can narrow (§4.3: "a deeper node cannot narrow… the only way to
  # withhold something an ancestor granted is to seal"). Anyone reading
  # `[registries.grants]` and expecting the absence of `releases:publish` to
  # withhold it is reading a deny rule that is not there.

  heavy_mark "additive"
  heavy_log "grants only widen — the denied puller can still publish, and that is the model"

  rc="$(authz_npm_publish "$T_DENIED" "widening-probe-$HEAVY_RUN" 1.0.0)"
  [[ "$rc" == 2* ]] || heavy_fail \
    "a role:user was refused an unsealed publish with $rc — either §10 rule 5 stopped \
granting publish to role:user, or something in front of the engine is asserting a role"

  # ── 6. The subject forms ───────────────────────────────────────────────────

  heavy_mark "subjects"
  heavy_log "the subject forms — role:, user:, and the one that matches nothing"

  # `token:` parses, loads, and prints in `explain-config` — and matches no
  # subject: `Subject` has one variant, so `SubjectMatcher::Token` answers
  # `false` for everyone. The registry block grants that subject all three read
  # verbs and the effect of that grant is nil, which §1 above is what shows: no
  # credential in this config reads anything it did not get from a `user:` or a
  # `role:` grant.
  #
  # `explain` refuses the question rather than answering `deny`, which is the
  # right refusal and worth pinning: `deny` would read as "this token is not
  # permitted", when what is true is that no principal can *be* that subject yet.
  local token_explain
  token_explain="$(authz_status GET "$T_ADMIN" \
    "/api/v1/admin/authz/explain?registry=$NPM&subject=token%3Aauthz-release-bot&action=releases%3Aread&package=$PKG")"
  [[ "$token_explain" == "400" ]] || heavy_fail \
    "explain answered $token_explain for a 'token:' subject, expected 400 — a machine token \
is not a principal any credential resolves to yet (§4.3), and answering the question at all \
would report the absence of a subject as the denial of one"
  AUTHZ_CHECKS=$((AUTHZ_CHECKS + 1))

  # `role:` is `has_role_at_least`, not equality — an admin holds what a user
  # holds, which is what every existing config has always meant (§10 rule 1).
  authz_oracle releases:publish "$NPM" "role:admin" allow "$PKG"
  authz_oracle releases:publish "$NPM" "role:user" allow "$PKG"
  authz_oracle releases:read "$NPM" "*" deny "$PKG"

  # ── 7. Shadow mode ─────────────────────────────────────────────────────────
  #
  # Two npm registries with identical grant blocks; one carries a
  # `[registries.grants_shadow]`. The same token is refused by the first and
  # served by the second, and the refusal that did not happen is on record.

  heavy_mark "shadow"
  heavy_log "shadow mode — the grants refuse, the request is served, the would-have-been is recorded"

  local shadow_body="$HEAVY_WORK/shadow-publish.json"
  python3 - "$SHADOW_PKG" > "$shadow_body" <<'PY'
import base64, json, sys
name = sys.argv[1]
tarball = base64.b64encode(b"authz-heavy-probe").decode()
print(json.dumps({
    "name": name,
    "versions": {"1.0.0": {"name": name, "version": "1.0.0", "dist": {"shasum": "0" * 40}}},
    "_attachments": {f"{name.split('/')[-1]}-1.0.0.tgz": {
        "content_type": "application/octet-stream", "data": tarball, "length": 17}},
}))
PY
  rc="$(authz_status PUT "$T_ADMIN" "/proxy/$SHADOW/${SHADOW_PKG//\//%2f}" \
    -H "$HDR_JSON" --data-binary @"$shadow_body")"
  [[ "$rc" == 2* ]] || heavy_fail "seeding the shadowed registry failed with $rc"

  authz_allowed source:read "a caller the grants refuse is served under a shadow" \
    GET "$T_DENIED" "/proxy/$SHADOW/${SHADOW_PKG//\//%2f}/1.0.0/tarball"

  # `explain` must say both things: the grants refuse, and the shadow serves it.
  # Folding either into the other is the §13.7 bug — a diagnostic contradicting
  # the server it describes.
  local explained
  explained="$(authz_body GET "$T_ADMIN" \
    "/api/v1/admin/authz/explain?registry=$SHADOW&subject=user%3Aauthz-denied&action=source%3Aread&package=%40shadowed%2Fprobe-$HEAVY_RUN")"
  python3 -c '
import json, sys
d = json.loads(sys.stdin.read())
decision = d.get("decision")
assert decision == "deny", "explain should still report the grants refusing, got " + repr(decision)
assert d.get("shadowed_by"), "explain reported no shadowed_by, so it contradicts a server that served the request"
' <<<"$explained" || heavy_fail "explain does not report the shadow beside the denial (§4.7, §13.7)"
  AUTHZ_CHECKS=$((AUTHZ_CHECKS + 1))

  # And the shadow log has it. A shadow whose would-have-beens are not visible
  # is failing open with no way to find out.
  local shadow_log
  shadow_log="$(authz_body GET "$T_ADMIN" "/api/v1/admin/authz/shadow")"
  python3 -c '
import json, sys
d = json.loads(sys.stdin.read())
assert not d.get("no_shadow_configured"), "the server reports no shadow configured, but the config carries one"
assert d.get("recent"), "the shadow served a refusal and recorded nothing"
' <<<"$shadow_log" || heavy_fail "the shadow log did not record the denial it served (§4.7)"
  AUTHZ_CHECKS=$((AUTHZ_CHECKS + 1))

  # ── 8. catalogue:browse ────────────────────────────────────────────────────
  #
  # Not `releases:list`, and the distinction is load-bearing: browsing a
  # catalogue in a console and resolving a version from a package manager are
  # different exposures. "Build agents resolve everything, people browse
  # nothing" is a real configuration and one verb cannot express it.

  heavy_mark "browse"
  heavy_log "catalogue:browse — the console surface, which is not a listing"

  # **It filters, it does not refuse** — so the assertion is on the document, not
  # on the status. A `403` would tell a caller the catalogue exists and has
  # something in it; `200` with an empty page and `total: 0` tells them nothing,
  # and it is §4.4 rule 1 done correctly: the total is computed over the filtered
  # set rather than over the rows they may not see. An accurate count over a
  # wrong scope is not a smaller bug than no count — it is what makes the wrong
  # scope worth exploiting (survey finding 2).
  #
  # Checking the status alone here would pass against a server that discloses the
  # whole catalogue to everyone, which is the one outcome this section exists to
  # rule out.
  explore_as() {  # token -> echoes "<status> <item-count> <total>"
    local tok="$1" body status
    status="$(authz_status GET "$tok" "/api/v1/explore/packages")"
    body="$(authz_body GET "$tok" "/api/v1/explore/packages")"
    echo "$status $(python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    print(len(d.get("items", [])), d.get("total"))
except Exception:
    print("? ?")
' <<<"$body")"
    return 0
  }

  authz_note_verb catalogue:browse
  local seen
  seen="$(explore_as "$T_BROWSER")"
  [[ "$seen" == "200 0 0" ]] && heavy_fail \
    "the caller holding catalogue:browse sees an empty catalogue — the positive control: \
without it, every assertion below passes against a server that discloses nothing to anyone \
because the surface is broken"
  [[ "$seen" == 200\ * ]] || heavy_fail "the browser got '$seen' from explore, expected a 200"
  AUTHZ_CHECKS=$((AUTHZ_CHECKS + 1))

  local who
  for who in "$T_READER" "$T_DENIED" "$T_ANON"; do
    seen="$(explore_as "$who")"
    [[ "$seen" == "200 0 0" ]] || heavy_fail \
      "a caller without catalogue:browse got '$seen' from the explore catalogue, expected \
\"200 0 0\" — an empty page whose total is also zero. Anything else is a disclosure: the \
reader holds every read verb and still must not enumerate the catalogue (§4.2), and the count \
must be taken over the filtered set (§4.4 rule 1)."
    AUTHZ_CHECKS=$((AUTHZ_CHECKS + 1))
  done

  # ── 9. The write verbs ─────────────────────────────────────────────────────
  #
  # Until §13.10 these were translated, stored and reported by `explain` while
  # being requested by **no route** — so a grants block withholding publish
  # changed nothing about who could publish, and `explain` answered `deny` for a
  # request the server served.

  heavy_mark "writes"
  heavy_log "the write verbs — on the request path, not only in the model"

  # Publish needs an identified principal: an anonymous publish creates an
  # owner-less package. That is not a role check, which is why it survived the
  # deletion of all nine `has_role_at_least` assertions.
  authz_note_verb releases:publish
  rc="$(authz_npm_publish "$T_ANON" "anon-probe-$HEAVY_RUN" 1.0.0)"
  if [[ "$rc" != "401" && "$rc" != "403" ]]; then
    heavy_fail "an anonymous publish got $rc — publish keeps one identified-principal check, \
because an anonymous publish creates an owner-less package and \`can_publish\` answers true for one"
  fi
  AUTHZ_CHECKS=$((AUTHZ_CHECKS + 1))

  authz_allowed releases:yank "the administrator yanks" \
    POST "$T_ADMIN" "/api/v1/admin/registries/$NPM/bulk-yank" \
    -H "$HDR_JSON" \
    --data "{\"packages\":[{\"name\":\"$PKG\",\"version\":\"1.0.0\"}]}"
  authz_denied releases:yank "$WHO_DENIED yanks" \
    POST "$T_DENIED" "/api/v1/admin/registries/$NPM/bulk-yank" \
    -H "$HDR_JSON" \
    --data "{\"packages\":[{\"name\":\"$PKG\",\"version\":\"1.0.0\"}]}"

  authz_allowed releases:delete "the administrator deletes" \
    POST "$T_ADMIN" "/api/v1/admin/packages/delete" \
    -H "$HDR_JSON" \
    --data "{\"registry\":\"$NPM\",\"name\":\"gone-$HEAVY_RUN\",\"version\":\"1.0.0\"}"
  authz_denied releases:delete "$WHO_DENIED deletes" \
    POST "$T_DENIED" "/api/v1/admin/packages/delete" \
    -H "$HDR_JSON" \
    --data "{\"registry\":\"$NPM\",\"name\":\"$PKG\",\"version\":\"1.0.0\"}"

  # `releases:overwrite` is consumed at exactly `immutable`'s scope, and this
  # server refuses every republish except on Maven's multi-file path (§13.6), so
  # what is assertable here is the refusal rather than a replacement. The verb
  # is reached: a republish resolves it before it resolves immutability.
  authz_note_verb releases:overwrite
  rc="$(authz_npm_publish "$T_DENIED" "$PKG" 1.0.0)"
  if [[ "$rc" == "200" || "$rc" == "201" ]]; then
    heavy_fail "republishing an existing version succeeded ($rc) — immutability is not being enforced"
  fi
  AUTHZ_CHECKS=$((AUTHZ_CHECKS + 1))

  # ── 10. Governance ─────────────────────────────────────────────────────────

  heavy_mark "governance"
  heavy_log "the governance verbs"

  authz_allowed owners:read "the administrator reads owners" \
    GET "$T_ADMIN" "/api/v1/admin/registries/$NPM/packages/$PKG/owners"
  authz_denied owners:read "$WHO_DENIED reads owners" \
    GET "$T_DENIED" "/api/v1/admin/registries/$NPM/packages/$PKG/owners"

  authz_allowed owners:write "the administrator adds an owner" \
    POST "$T_ADMIN" "/api/v1/admin/registries/$NPM/packages/$PKG/owners" \
    -H "$HDR_JSON" --data '{"principal_type":"user","principal_id":"authz-reader"}'
  authz_denied owners:write "$WHO_DENIED adds an owner" \
    POST "$T_DENIED" "/api/v1/admin/registries/$NPM/packages/$PKG/owners" \
    -H "$HDR_JSON" --data '{"principal_type":"user","principal_id":"authz-denied"}'

  authz_allowed packages:block "the administrator blocks" \
    POST "$T_ADMIN" "/api/v1/admin/packages/block" \
    -H "$HDR_JSON" \
    --data "{\"registry\":\"$NPM\",\"name\":\"$PKG\",\"version\":\"1.0.0\",\"reason\":\"heavy authz probe\"}"
  authz_denied packages:block "$WHO_DENIED blocks" \
    POST "$T_DENIED" "/api/v1/admin/packages/block" \
    -H "$HDR_JSON" \
    --data "{\"registry\":\"$NPM\",\"name\":\"$PKG\",\"version\":\"1.0.0\",\"reason\":\"nope\"}"

  authz_allowed packages:read "the administrator reads the inventory" \
    GET "$T_ADMIN" "/api/v1/admin/packages"
  authz_denied packages:read "$WHO_DENIED reads the inventory" \
    GET "$T_DENIED" "/api/v1/admin/packages"

  # §4.4's boundary for an aggregate, made explicit: **held nowhere is a 403,
  # held somewhere filters.** The `403` arm is the one that matters, and it is
  # fragile in a way worth stating — `visible_registries` refuses only when the
  # filtered set comes out *empty*, so anything that makes one registry resolve
  # `stats:read` for everybody removes the refusal for everybody. A registry-tier
  # `grants_shadow` does exactly that; this config keeps its shadow on a
  # namespace so the arm below is testing the gate rather than the shadow. See
  # the note in config.authz.toml.
  authz_allowed stats:read "the administrator reads the aggregates" \
    GET "$T_ADMIN" "/api/v1/admin/stats"
  authz_denied stats:read "$WHO_DENIED reads the aggregates" \
    GET "$T_DENIED" "/api/v1/admin/stats"
  authz_refused_anonymous stats:read "an unauthenticated caller reads the aggregates" \
    GET "/api/v1/admin/stats"

  # `gates:exempt` is granted to **nobody** by any translation rule — not even
  # to `role:admin`, deliberately (§4.5), so that silencing a gate finding is a
  # decision an estate makes rather than one it inherits. This is the only
  # assertion in the suite where the administrator is the negative arm.
  heavy_log "gates:exempt — the one verb the administrator does not hold"
  authz_denied gates:exempt "the administrator, who holds every other verb" \
    PUT "$T_ADMIN" "/api/v1/admin/registries/$NPM/policy/version/$PKG/1.0.0/rules/cve_gate" \
    -H "$HDR_JSON" \
    --data '{"reason":"heavy authz probe","exempt_until":"2030-01-01T00:00:00Z"}'
  authz_allowed gates:exempt "the token the config grants it to" \
    PUT "$T_GATE" "/api/v1/admin/registries/$NPM/policy/version/$PKG/1.0.0/rules/cve_gate" \
    -H "$HDR_JSON" \
    --data '{"reason":"heavy authz probe","exempt_until":"2030-01-01T00:00:00Z"}'

  # ── 11. The audit pair ─────────────────────────────────────────────────────
  #
  # An estate that wants a reviewer to *read* the trail does not thereby want
  # them able to erase it — including the record of their own actions, and
  # including the `audit:purge` event the purge itself writes.

  heavy_mark "audit"
  heavy_log "audit:read / audit:purge — the split that makes the delegation safe"

  authz_allowed audit:read "the auditor reads the log" \
    GET "$T_AUDITOR" "/api/v1/admin/audit-log"
  authz_denied audit:read "$WHO_DENIED reads the log" \
    GET "$T_DENIED" "/api/v1/admin/audit-log"
  authz_denied audit:purge "the auditor erases the log" \
    DELETE "$T_AUDITOR" "/api/v1/admin/audit-log?before=2020-01-01T00:00:00Z"
  authz_allowed audit:purge "the administrator erases the log" \
    DELETE "$T_ADMIN" "/api/v1/admin/audit-log?before=2020-01-01T00:00:00Z"

  # ── 12. The quota pair ─────────────────────────────────────────────────────

  heavy_mark "quota"
  heavy_log "quota:read / quota:write — a support engineer inspects, and does not zero"

  authz_allowed quota:read "support reads quota usage" \
    GET "$T_SUPPORT" "/api/v1/admin/quota"
  authz_denied quota:read "$WHO_DENIED reads quota usage" \
    GET "$T_DENIED" "/api/v1/admin/quota"
  authz_denied quota:write "support resets a user's counters" \
    DELETE "$T_SUPPORT" "/api/v1/admin/quota/$NPM/authz-reader"
  authz_allowed quota:write "the administrator resets them" \
    DELETE "$T_ADMIN" "/api/v1/admin/quota/$NPM/authz-reader"

  # ── 12-bis. The grants pair, and the tier it writes (RFC 0017) ─────────────
  #
  # The editor for the two tiers a config file cannot enumerate. Three things
  # are asserted, in the order they can fail:
  #
  #   1. **The pair is split.** `grants:read` does not confer `grants:write`.
  #      Open question 2 closed them as separate verbs because a grant listing
  #      enumerates every subject that can reach a private package — the larger
  #      of the two disclosures, and not the one `audit:read` covers.
  #   2. **The version tier is writable at all.** Migration 041 has carried
  #      `node_kind = 'version'` since RFC 0015 and nothing could populate it;
  #      the whole RFC is that this `PUT` exists.
  #   3. **`explain` agrees about the row that was just written.** This is the
  #      one that failed. §6.3 said the diagnostic needed no change because it
  #      "resolves through the same path"; it did not — the stored tiers are
  #      appended after a short-circuit no diagnostic reaches, so `explain`
  #      answered `deny` about a grant the server honours.

  heavy_mark "grants"
  heavy_log "grants:read / grants:write — RFC 0017's editor, and the version tier it writes"

  authz_allowed grants:read "the grants reader lists a package's grants" \
    GET "$T_GRANTS_READER" "/api/v1/admin/registries/$NPM/grants?package=$PKG"
  authz_denied grants:read "$WHO_DENIED lists them" \
    GET "$T_DENIED" "/api/v1/admin/registries/$NPM/grants?package=$PKG"
  authz_denied grants:write "the grants reader writes one" \
    PUT "$T_GRANTS_READER" "/api/v1/admin/registries/$NPM/grants" \
    -H "$HDR_JSON" \
    --data "{\"package\":\"$PKG\",\"subject\":\"user:$GRANTEE\",\"actions\":[\"releases:read\"]}"

  # The version tier, written through the route and read back through the
  # diagnostic. `$GRANTEE` holds nothing anywhere else in this file, so an
  # `allow` here is attributable to this row and to nothing that came before it.
  authz_allowed grants:write "the administrator writes a version-tier grant" \
    PUT "$T_ADMIN" "/api/v1/admin/registries/$NPM/grants" \
    -H "$HDR_JSON" \
    --data "{\"package\":\"$PKG\",\"version\":\"1.0.0\",\"subject\":\"user:$GRANTEE\",\"actions\":[\"releases:read\"]}"

  authz_oracle releases:read "$NPM" "user:$GRANTEE" allow "$PKG" "1.0.0"
  # Grants only widen, and they widen *downward from where they are written*: a
  # row on `1.0.0` is not a row on the package, and the package question has to
  # keep answering deny or the tier means nothing.
  authz_oracle releases:read "$NPM" "user:$GRANTEE" deny "$PKG"

  # ── 13. The instance tier ──────────────────────────────────────────────────
  #
  # About a dozen control endpoints name no registry, and §4.1's hierarchy
  # started at `registry` — so `instance` is a fifth tier added above it. Both
  # diagnostics answered `deny` where the server answered `allow` until the path
  # builder could see it (§13.9), which is why the oracle is asked here too.

  heavy_mark "instance"
  heavy_log "the instance tier — verbs held above every registry"

  authz_allowed system:read "the SRE reads health" \
    GET "$T_SRE" "/api/v1/admin/health"
  authz_denied system:read "$WHO_DENIED reads health" \
    GET "$T_DENIED" "/api/v1/admin/health"
  # The body is not optional: `web::Json<ExploreInvalidateRequest>` is an
  # extractor, so it runs before the handler and a missing one is a `400` that
  # never reaches the gate — which would make the negative arm below pass
  # without authorization having been consulted at all.
  authz_denied system:write "the SRE, who holds only the read half" \
    POST "$T_SRE" "/api/v1/admin/explore/invalidate" \
    -H "$HDR_JSON" --data '{}'
  authz_allowed system:write "the administrator" \
    POST "$T_ADMIN" "/api/v1/admin/explore/invalidate" \
    -H "$HDR_JSON" --data '{}'

  authz_allowed config:read "the administrator reads the warnings" \
    GET "$T_ADMIN" "/api/v1/admin/config/warnings"
  authz_denied config:read "the SRE, whose instance grant does not include it" \
    GET "$T_SRE" "/api/v1/admin/config/warnings"
  authz_allowed config:write "the administrator reloads" \
    POST "$T_ADMIN" "/api/v1/admin/config/reload"
  authz_denied config:write "$WHO_DENIED reloads" \
    POST "$T_DENIED" "/api/v1/admin/config/reload"

  authz_allowed blocks:read "the administrator lists IP blocks" \
    GET "$T_ADMIN" "$IP_BLOCKS"
  authz_denied blocks:read "$WHO_DENIED lists IP blocks" \
    GET "$T_DENIED" "$IP_BLOCKS"
  authz_allowed blocks:write "the administrator places one" \
    POST "$T_ADMIN" "$IP_BLOCKS" \
    -H "$HDR_JSON" \
    --data '{"ip":"192.0.2.1","reason":"heavy authz probe"}'
  authz_denied blocks:write "$WHO_DENIED places one" \
    POST "$T_DENIED" "$IP_BLOCKS" \
    -H "$HDR_JSON" --data '{"ip":"192.0.2.2","reason":"nope"}'

  authz_allowed authz:read "the administrator probes the resolver" \
    GET "$T_ADMIN" "/api/v1/admin/authz/shadow"
  authz_denied authz:read "the auditor, who may read the trail but not the resolver" \
    GET "$T_AUDITOR" "/api/v1/admin/authz/shadow"

  # `cache:evict` is granted at the instance tier and the endpoint names a
  # registry: the path is [instance, registry] and a grant at either passes.
  # That composition is the reason both scopes exist rather than one.
  authz_allowed cache:evict "the SRE, holding it at the instance tier, evicts on a registry" \
    POST "$T_SRE" "/api/v1/admin/registries/$NPM/clear-cache"
  authz_denied cache:evict "$WHO_DENIED" \
    POST "$T_DENIED" "/api/v1/admin/registries/$NPM/clear-cache"

  authz_allowed cache:warm "the administrator reads the warming state" \
    GET "$T_ADMIN" "/api/v1/admin/warming"
  authz_denied cache:warm "$WHO_DENIED" \
    GET "$T_DENIED" "/api/v1/admin/warming"

  authz_allowed retention:run "the administrator runs retention" \
    POST "$T_ADMIN" "/api/v1/admin/registries/$NPM/retention?dry_run=true"
  authz_denied retention:run "$WHO_DENIED" \
    POST "$T_DENIED" "/api/v1/admin/registries/$NPM/retention?dry_run=true"

  authz_allowed tombstones:read "the administrator reads tombstones" \
    GET "$T_ADMIN" "/api/v1/admin/registries/$NPM/tombstones"
  authz_denied tombstones:read "$WHO_DENIED" \
    GET "$T_DENIED" "/api/v1/admin/registries/$NPM/tombstones"

  # `flags:read` (RFC 0002 recast, §13). Instance-wide as `audit:read` is: the
  # listing spans every registry, so the filter narrows what is shown and not
  # who may look. It is `role:admin` only — a pushed flag names its source, and
  # naming the source is the same disclosure as a finding's SOC case — so the
  # denied user, who is `role:user`, is the negative arm without any grant row
  # having to withhold it.
  authz_allowed flags:read "the administrator lists pushed flags" \
    GET "$T_ADMIN" "/api/v1/admin/flags"
  authz_denied flags:read "$WHO_DENIED, who is role:user" \
    GET "$T_DENIED" "/api/v1/admin/flags"

  # ── 14. The ecosystem verbs ────────────────────────────────────────────────
  #
  # All three shipped **unreachable**: no §10 rule produces a verb no legacy
  # config means, so each was held by nobody and every one of these endpoints
  # answered `403` to the administrator, with passing tests, because each test
  # granted its own verb. The positive arm below is the control that caught it.

  heavy_mark "ecosystem"
  heavy_log "the ecosystem verbs — each with the positive control that found them unreachable"

  authz_allowed terraform:signing-keys:write "the administrator registers a signing key" \
    PUT "$T_ADMIN" "/api/v1/admin/registries/$TFREG/signing-keys/hashicorp" \
    -H "$HDR_JSON" \
    --data '{"key_id":"51852D87348FFC4C","ascii_armor":"-----BEGIN PGP PUBLIC KEY BLOCK-----\nnot-a-key\n-----END PGP PUBLIC KEY BLOCK-----"}'
  authz_denied terraform:signing-keys:write "$WHO_DENIED" \
    PUT "$T_DENIED" "/api/v1/admin/registries/$TFREG/signing-keys/hashicorp" \
    -H "$HDR_JSON" --data '{"key_id":"x","ascii_armor":"x"}'

  authz_allowed jetbrains:channel:assign "the administrator assigns a channel" \
    PUT "$T_ADMIN" "/api/v1/admin/registries/$JB/plugins/com.example.probe/1.0.0/channel" \
    -H "$HDR_JSON" --data '{"channel":"eap"}'
  authz_denied jetbrains:channel:assign "$WHO_DENIED" \
    PUT "$T_DENIED" "/api/v1/admin/registries/$JB/plugins/com.example.probe/1.0.0/channel" \
    -H "$HDR_JSON" --data '{"channel":"eap"}'

  # Administrative rather than self-service, deliberately: a namespace is a tier
  # grants are written on, so upstream's first-come model would let a user mint
  # the scope their own permissions are then read from.
  authz_allowed openvsx:namespace:claim "the administrator claims a namespace" \
    POST "$T_ADMIN" "/proxy/$VSX/api/-/namespace/create?name=heavyorg" \
    -H "$HDR_JSON" --data '{"group_id":"heavy"}'
  authz_denied openvsx:namespace:claim "a user claims one for themselves" \
    POST "$T_DENIED" "/proxy/$VSX/api/-/namespace/create?name=heavygrab" \
    -H "$HDR_JSON" --data '{"group_id":"heavy"}'

  # ── 15. Listings filter, they do not refuse (§4.4) ─────────────────────────
  #
  # A whole-registry document is the one place a grant boundary must *not* be a
  # status code: the caller gets what they may see. Composer's
  # `available-packages` is the document with no per-package fallback, which is
  # why a namespace seal leaking into it is visible here and nowhere else.

  heavy_mark "filter"
  heavy_log "§4.4 — a whole-registry document filters rather than refusing"

  # Composer's `packages.json` is one of the two routes that request
  # `releases:list` directly, so this is the pair the verb has: held, the
  # document is served and filtered; not held, it is refused.
  local composer_status
  composer_status="$(authz_status GET "$T_LISTER" "/proxy/$COMPOSER_REG/packages.json")"
  [[ "$composer_status" == "200" ]] || heavy_fail \
    "the lister got $composer_status from a whole-registry document — §4.4 is explicit that \
a listing filters rather than refusing, and that an empty filtered result is 200 with an \
empty document"
  authz_note_verb releases:list
  AUTHZ_CHECKS=$((AUTHZ_CHECKS + 1))

  authz_denied releases:list "$WHO_DENIED asks for the whole-registry document" \
    GET "$T_DENIED" "/proxy/$COMPOSER_REG/packages.json"

  heavy_log "MATRIX-OK ($AUTHZ_CHECKS assertions)"
  return 0
}

# ═════════════════════════════════════════════════════════════════════════════
# The client phases — the boundary as a package manager meets it
# ═════════════════════════════════════════════════════════════════════════════

# Each is the same three-part shape, and the shape is the point:
#
#   seed   the administrator publishes, so there is something to be refused
#   deny   the denied identity drives the real client — it must FAIL, and the
#          wire must show a 403 rather than a 404 or a silent empty answer
#   allow  the permitted identity drives the identical command — it must WORK
#
# Without the third part the second proves only that something went wrong.

phase_npm() {
  heavy_need npm "nodejs"
  heavy_log "npm $(npm --version)"

  local base="$HEAVY_TAP_BASE/proxy/$NPM/"

  npmrc_for() {  # token, suffix -> echoes the file path
    local token="$1" suffix="$2"
    local file="$HEAVY_WORK/npmrc-$suffix"
    # npm keys auth by `//host/path/`; a token on the wrong key is silently no
    # token at all, and the request then goes out anonymous — which would make
    # every denial below pass for the wrong reason.
    local host_key="//127.0.0.1:$HEAVY_TAP_PORT/proxy/$NPM/"
    {
      echo "registry=$base"
      if [[ "$token" != "$T_ANON" ]]; then
        echo "${host_key}:_authToken=$token"
      fi
    } > "$file"
    echo "$file"
    return 0
  }

  export NPM_CONFIG_FUND=false NPM_CONFIG_AUDIT=false NPM_CONFIG_UPDATE_NOTIFIER=false

  make_pkg() {  # dir, name, version
    local dir="$1" name="$2" version="$3"
    mkdir -p "$dir"
    cat > "$dir/package.json" <<EOF
{ "name": "$name", "version": "$version", "description": "RFC 0015 heavy authz probe",
  "license": "MIT", "main": "index.js" }
EOF
    echo "module.exports = '$name';" > "$dir/index.js"
    return 0
  }

  # ── seed ──
  heavy_mark "npm-seed"
  make_pkg "$HEAVY_WORK/pkg" "$PKG" 1.0.0
  make_pkg "$HEAVY_WORK/team" "$TEAM_PKG" 1.0.0
  NPM_CONFIG_USERCONFIG="$(npmrc_for "$T_ADMIN" admin)" \
    NPM_CONFIG_CACHE="$HEAVY_WORK/npm-seed-cache" \
    bash -c "cd '$HEAVY_WORK/pkg' && npm publish --registry '$base'" \
    >"$HEAVY_WORK/npm-seed.log" 2>&1 \
    || { tail -20 "$HEAVY_WORK/npm-seed.log" >&2; heavy_fail "seeding $PKG with npm publish failed"; }
  NPM_CONFIG_USERCONFIG="$(npmrc_for "$T_ADMIN" admin)" \
    NPM_CONFIG_CACHE="$HEAVY_WORK/npm-seed-cache" \
    bash -c "cd '$HEAVY_WORK/team' && npm publish --access public --registry '$base'" \
    >>"$HEAVY_WORK/npm-seed.log" 2>&1 \
    || { tail -20 "$HEAVY_WORK/npm-seed.log" >&2; heavy_fail "seeding $TEAM_PKG with npm publish failed"; }

  # ── deny ──
  #
  # Each install gets its own cache. `npm publish` seeds the tarball it packed
  # into cacache, and a shared cache would let an install succeed without the
  # server being asked — which for a denial test means passing while the
  # boundary does not exist.
  heavy_mark "npm-deny"
  heavy_log "npm install as the denied user — must fail"
  set +e
  NPM_CONFIG_USERCONFIG="$(npmrc_for "$T_DENIED" denied)" \
    NPM_CONFIG_CACHE="$HEAVY_WORK/npm-deny-cache" \
    npm install --prefix "$HEAVY_WORK/consumer-deny" --no-save "$PKG@1.0.0" \
    >"$HEAVY_WORK/npm-deny.log" 2>&1
  local deny_rc=$?
  set -e
  if [[ $deny_rc -eq 0 ]]; then
    cat "$HEAVY_WORK/npm-deny.log" >&2
    heavy_fail "npm install SUCCEEDED for a caller holding no read verb — the boundary is not there"
  fi
  heavy_wire_after "npm-deny" "$WIRE_403" \
    "npm was stopped, but no 403 appears in the transcript after the denial phase — it failed \
for some other reason, which is a denial that proves nothing"
  [[ -d "$HEAVY_WORK/consumer-deny/node_modules/$PKG" ]] \
    && heavy_fail "npm exited non-zero and installed the package anyway"

  # ── allow ──
  heavy_mark "npm-allow"
  heavy_log "npm install as the reader — must succeed"
  NPM_CONFIG_USERCONFIG="$(npmrc_for "$T_READER" reader)" \
    NPM_CONFIG_CACHE="$HEAVY_WORK/npm-allow-cache" \
    npm install --prefix "$HEAVY_WORK/consumer-allow" --no-save "$PKG@1.0.0" \
    >"$HEAVY_WORK/npm-allow.log" 2>&1 \
    || { cat "$HEAVY_WORK/npm-allow.log" >&2; heavy_fail "npm install failed for the reader — the positive control"; }
  [[ -d "$HEAVY_WORK/consumer-allow/node_modules/$PKG" ]] \
    || heavy_fail "npm reported success and installed nothing"

  # ── the namespace tier, through the client ──
  #
  # `npm view`, not `npm install`, and the reason is a bug this suite found
  # rather than a weaker claim: **a scoped package published to a local registry
  # is not installable at all**, for any caller. The packument's `dist.tarball`
  # is written with the scope separator unencoded —
  # `…/proxy/{reg}/@team/probe/1.0.0/tarball` — and the download route takes the
  # package as **one** path segment, so the URL the server hands npm is four
  # segments and matches nothing. It answers `404`; the `%2f` spelling answers
  # `200`. Reproduce with `npm publish` of any `@scope/name` and `npm install`
  # of it.
  #
  # That is a routing defect, not an authorization one, and this suite must not
  # assert it either way. So the namespace arm is made on the packument, which
  # the client really does fetch and which the registry-tier denial really would
  # have refused: the same token that cannot resolve `$PKG` resolves
  # `$TEAM_PKG`. When the tarball URL is fixed, replace this with the
  # `npm install` above and the arm gets stronger for free.
  heavy_mark "npm-namespace"
  heavy_log "npm view of the namespace-granted scope, as the denied user — must succeed"
  local viewed
  viewed="$(NPM_CONFIG_USERCONFIG="$(npmrc_for "$T_DENIED" denied)" \
    NPM_CONFIG_CACHE="$HEAVY_WORK/npm-ns-cache" \
    npm view "$TEAM_PKG" version --registry "$base" 2>"$HEAVY_WORK/npm-ns.log")" \
    || { cat "$HEAVY_WORK/npm-ns.log" >&2; heavy_fail \
      "the namespace grant did not reach the client — the same token was refused $PKG and must be served $TEAM_PKG"; }
  [[ "$viewed" == "1.0.0" ]] || heavy_fail "npm view reported '$viewed', expected 1.0.0"

  # …and the boundary the grant stops at, through the client too.
  set +e
  NPM_CONFIG_USERCONFIG="$(npmrc_for "$T_DENIED" denied)" \
    NPM_CONFIG_CACHE="$HEAVY_WORK/npm-nsx-cache" \
    npm view "$PKG" version --registry "$base" >"$HEAVY_WORK/npm-nsx.log" 2>&1
  local nsx_rc=$?
  set -e
  [[ $nsx_rc -ne 0 ]] || heavy_fail \
    "the denied user resolved $PKG, which no grant covers — the namespace grant is reaching \
outside its own subtree"

  heavy_log "NPM-AUTHZ-OK (refused, served, and the namespace grant visible to the client)"
  return 0
}

phase_pypi() {
  heavy_need python3 "python3 with venv"

  local upload="$HEAVY_TAP_BASE/proxy/$PYPI/legacy/"
  local dist="authz-probe-$HEAVY_RUN" module="authz_probe_$HEAVY_RUN"

  heavy_log "Creating the build virtualenv ($(python3 --version))"
  python3 -m venv "$HEAVY_WORK/venv" || heavy_fail "python3 -m venv failed (python3-venv missing?)"
  local vpy="$HEAVY_WORK/venv/bin/python"
  "$vpy" -m pip install --quiet --upgrade pip setuptools wheel build twine \
    || heavy_fail "could not install the build toolchain"

  local src="$HEAVY_WORK/src"
  mkdir -p "$src/$module"
  cat > "$src/pyproject.toml" <<EOF
[build-system]
requires = ["setuptools>=68"]
build-backend = "setuptools.build_meta"

[project]
name = "$dist"
version = "1.0.0"
description = "RFC 0015 heavy authz probe"
requires-python = ">=3.8"

[tool.setuptools]
packages = ["$module"]
EOF
  echo "VALUE = '$dist'" > "$src/$module/__init__.py"
  (cd "$src" && "$vpy" -m build --wheel --no-isolation) >"$HEAVY_WORK/build.log" 2>&1 \
    || { tail -30 "$HEAVY_WORK/build.log" >&2; heavy_fail "python -m build failed"; }

  heavy_mark "pypi-seed"
  TWINE_USERNAME="__token__" TWINE_PASSWORD="$T_ADMIN" \
    "$HEAVY_WORK/venv/bin/twine" upload --repository-url "$upload" \
    --disable-progress-bar "$src"/dist/*.whl \
    || heavy_fail "twine upload failed — the documented publish flow sends HTTP Basic, not Bearer"

  # pip carries the credential in the URL's userinfo, which is the shape the
  # registry page documents. `__token__` as the username matches twine's.
  local host="127.0.0.1:$HEAVY_TAP_PORT"
  local deny_index="http://__token__:$T_DENIED@$host/proxy/$PYPI/simple/"
  local allow_index="http://__token__:$T_READER@$host/proxy/$PYPI/simple/"

  heavy_mark "pypi-deny"
  heavy_log "pip install as the denied user — must fail"
  python3 -m venv "$HEAVY_WORK/deny-venv" || heavy_fail "consumer venv failed"
  set +e
  "$HEAVY_WORK/deny-venv/bin/python" -m pip install --no-cache-dir \
    --index-url "$deny_index" "$dist==1.0.0" >"$HEAVY_WORK/pip-deny.log" 2>&1
  local deny_rc=$?
  set -e
  [[ $deny_rc -ne 0 ]] || { cat "$HEAVY_WORK/pip-deny.log" >&2; \
    heavy_fail "pip install SUCCEEDED for a caller holding no read verb"; }
  heavy_wire_after "pypi-deny" "$WIRE_403" "pip was stopped without a 403 in the transcript"

  heavy_mark "pypi-allow"
  heavy_log "pip install as the reader — must succeed"
  python3 -m venv "$HEAVY_WORK/allow-venv" || heavy_fail "consumer venv failed"
  "$HEAVY_WORK/allow-venv/bin/python" -m pip install --no-cache-dir \
    --index-url "$allow_index" "$dist==1.0.0" >"$HEAVY_WORK/pip-allow.log" 2>&1 \
    || { tail -30 "$HEAVY_WORK/pip-allow.log" >&2; heavy_fail "pip install failed for the reader — the positive control"; }
  "$HEAVY_WORK/allow-venv/bin/python" -c "import $module; assert ${module}.VALUE == '$dist'" \
    || heavy_fail "the installed distribution is not the one that was published"

  # The simple page is a listing (`releases:list`) and the wheel is an artifact
  # (`releases:read`), so the lister gets one and not the other. This is the
  # split as pip meets it, and it is the reason the two verbs exist.
  heavy_mark "pypi-lister"
  local page_status
  page_status="$(authz_status GET "$T_LISTER" "/proxy/$PYPI/simple/$dist/")"
  [[ "$page_status" == "200" ]] || heavy_fail "the lister got $page_status from the simple page, expected 200"
  heavy_log "PYPI-AUTHZ-OK (refused, served, and the listing/artifact split visible)"
  return 0
}

phase_nuget() {
  local dotnet_version="${DOTNET_VERSION:-10}"
  heavy_runner_for dotnet "dotnet@$dotnet_version"
  local dotnet=("${HEAVY_RUNNER[@]}" dotnet)

  local index="$HEAVY_TAP_BASE/proxy/$NUGET/nuget/v3/index.json"
  local id="heavyauthz.probe$HEAVY_RUN"
  local sealed_id="heavysealed.probe$HEAVY_RUN"

  # **`dotnet restore` cannot authenticate to this server, so the read boundary
  # is not observable through it.** `packageSourceCredentials` are attached to
  # `HttpClientHandler.Credentials`, which .NET sends only in answer to a `401`
  # with a `WWW-Authenticate` header; BatleHub answers an unauthenticated read
  # `403` with no challenge, so the credential is never offered and a reader who
  # holds every read verb gets the same `403` as a caller who holds none.
  # Verified: `curl -u authz-reader:<token>` on the same URL is authorised, and
  # `ValidAuthenticationTypes = basic` does not change it — the challenge is what
  # is missing, not the scheme.
  #
  # So this phase asserts what the client *can* carry. `dotnet nuget push` sends
  # `--api-key` explicitly, so a push is attributable; and the one thing a push
  # can be refused for on a local registry is a **seal**, because §10 rule 5
  # hands `releases:publish` to every `role:user` and grants only widen. The read
  # boundary is asserted with Basic auth over curl, which is the credential the
  # client would send if it were asked for one.
  export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1
  export NUGET_PACKAGES="$HEAVY_WORK/nuget-packages"
  export NUGET_HTTP_CACHE_PATH="$HEAVY_WORK/nuget-http-cache"

  # `allowInsecureConnections` is not optional: NuGet refuses a plain-HTTP source
  # outright, on push as well as on restore (RFC 0009 §12.4). A real deployment
  # is HTTPS; a local one needs this line. NuGet walks up from the working
  # directory for `nuget.config`, so one file at the root of the work tree covers
  # every `dotnet` call below.
  cat > "$HEAVY_WORK/nuget.config" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <clear />
    <add key="batlehub" value="$index" allowInsecureConnections="true" />
  </packageSources>
</configuration>
EOF

  local sdk tfm
  sdk="$("${dotnet[@]}" --version)"
  tfm="net${sdk%%.*}.0"
  heavy_log "dotnet $sdk, targeting $tfm"

  pack() {  # id -> echoes the .nupkg path
    local pkg_id="$1"
    local dir="$HEAVY_WORK/src/$pkg_id"
    mkdir -p "$dir"
    cat > "$dir/$pkg_id.csproj" <<EOF
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>$tfm</TargetFramework>
    <PackageId>$pkg_id</PackageId>
    <Version>1.0.0</Version>
    <Authors>batlehub heavy tests</Authors>
    <Description>RFC 0015 heavy authz probe</Description>
  </PropertyGroup>
</Project>
EOF
    echo "namespace Probe { public static class Marker { public const string Id = \"$pkg_id\"; } }" \
      > "$dir/Marker.cs"
    (cd "$dir" && "${dotnet[@]}" pack -c Release -o "$dir/out") >>"$HEAVY_WORK/pack.log" 2>&1 \
      || { tail -30 "$HEAVY_WORK/pack.log" >&2; heavy_fail "dotnet pack failed for $pkg_id"; }
    echo "$dir/out/$pkg_id.1.0.0.nupkg"
    return 0
  }

  # ── the client, pushing where it may ──
  heavy_mark "nuget-push"
  heavy_log "dotnet nuget push into an open prefix — the positive control"
  local open_nupkg
  open_nupkg="$(pack "$id")"
  (cd "$HEAVY_WORK" && "${dotnet[@]}" nuget push "$open_nupkg" \
    --source "$index" --api-key "$T_ADMIN") >"$HEAVY_WORK/push.log" 2>&1 \
    || { tail -30 "$HEAVY_WORK/push.log" >&2; heavy_fail "dotnet nuget push failed — the positive control"; }
  heavy_wire_after "nuget-push" "PUT /proxy/$NUGET/nuget/api/v2/package/ -> 201" \
    "dotnet nuget push did not reach the publish endpoint"

  # ── the client, pushing where it may not ──
  heavy_mark "nuget-sealed"
  heavy_log "dotnet nuget push into the sealed prefix — must fail, for everybody"
  local sealed_nupkg
  sealed_nupkg="$(pack "$sealed_id")"
  set +e
  (cd "$HEAVY_WORK" && "${dotnet[@]}" nuget push "$sealed_nupkg" \
    --source "$index" --api-key "$T_ADMIN") >"$HEAVY_WORK/push-sealed.log" 2>&1
  local sealed_rc=$?
  set -e
  [[ $sealed_rc -ne 0 ]] || { tail -30 "$HEAVY_WORK/push-sealed.log" >&2; heavy_fail \
    "dotnet nuget push put a package into a sealed prefix — a seal that role:admin walks \
through is not a seal"; }
  heavy_wire_after "nuget-sealed" "$WIRE_403" \
    "the push was rejected without a 403 on the wire, so it failed for some other reason"

  # ── the read boundary, over curl for the reason above ──
  heavy_mark "nuget-read"
  heavy_log "the flat index — refused and served, with the credential NuGet cannot be made to send"
  local flat="/proxy/$NUGET/nuget/v3/flat/$id/index.json"
  local denied_status reader_status anon_status
  denied_status="$(authz_status GET "$T_DENIED" "$flat")"
  reader_status="$(authz_status GET "$T_READER" "$flat")"
  anon_status="$(authz_status GET "$T_ANON" "$flat")"
  [[ "$denied_status" == "403" ]] || heavy_fail "the denied caller got $denied_status, expected 403"
  [[ "$reader_status" != "403" && "$reader_status" != "401" ]] || heavy_fail \
    "the reader got $reader_status for the flat index — the positive control"
  [[ "$anon_status" == "403" ]] || heavy_fail "an unauthenticated caller got $anon_status, expected 403"

  # The finding above, pinned so it cannot quietly change under the comment: an
  # unauthenticated read is refused **without a challenge**, which is what keeps
  # `dotnet restore` from ever offering its credential. If a `WWW-Authenticate`
  # header appears here, the restore arms in this phase can and should come back.
  local challenge
  challenge="$(curl -s -D - -o /dev/null "$HEAVY_TAP_BASE$flat" | grep -ci '^www-authenticate' || true)"
  [[ "$challenge" == "0" ]] || heavy_log \
    "NOTE: the anonymous 403 now carries WWW-Authenticate — NuGet may be able to \
authenticate, so restore the dotnet restore arms this phase gave up on."

  heavy_log "NUGET-AUTHZ-OK (a seal the client meets, and the read boundary beside it)"
  return 0
}

phase_composer() {
  local composer_version="${COMPOSER_VERSION:-2.10.2}"
  local static_php_version="${STATIC_PHP_VERSION:-8.3.28}"

  # The runner image usually has both. `mise`'s PHP backends compile from
  # source, which is not something a test may assume, so the fallback is the
  # static build RFC 0009 §12.5 used.
  local php
  if php --version >/dev/null 2>&1; then
    php="$(command -v php)"
  else
    local php_dir
    php_dir="$(heavy_cached_dir "static-php-$static_php_version" \
      "https://dl.static-php.dev/static-php-cli/common/php-$static_php_version-cli-linux-x86_64.tar.gz" tar.gz)"
    php="$php_dir/php"
    [[ -x "$php" ]] || heavy_fail "the static PHP archive did not contain a php binary at $php"
  fi

  local composer=()
  if command -v composer >/dev/null 2>&1 && composer --version >/dev/null 2>&1; then
    composer=(composer)
  else
    local phar="$HEAVY_CACHE/composer-$composer_version.phar"
    if [[ ! -f "$phar" ]]; then
      mkdir -p "$HEAVY_CACHE"
      curl -fsSL --proto '=https' --proto-redir '=https' \
        -o "$phar" "https://getcomposer.org/download/$composer_version/composer.phar" \
        || heavy_fail "could not download composer.phar $composer_version"
    fi
    composer=("$php" "$phar")
  fi

  export COMPOSER_HOME="$HEAVY_WORK/composer-home"
  export COMPOSER_NO_INTERACTION=1
  mkdir -p "$COMPOSER_HOME"

  local url="$HEAVY_TAP_BASE/proxy/$COMPOSER_REG"
  local host="127.0.0.1:$HEAVY_TAP_PORT"
  local pkg="heavyauthz/p$HEAVY_RUN"
  local zip="$HEAVY_WORK/probe.zip"

  python3 tests/heavy/make_composer_zip.py "$zip" "$pkg" 1.0.0 \
    || heavy_fail "building the composer zip failed"

  heavy_mark "composer-seed"
  curl -fsS -o /dev/null -X POST -H "Authorization: Bearer $T_ADMIN" \
    --data-binary @"$zip" "$url/api/upload" || heavy_fail "publishing failed"

  # `secure-http: false` because this instance is plain HTTP, and
  # `packagist.org: false` because Composer adds it implicitly and a resolve it
  # satisfies is a resolve BatleHub was not asked for — which for a *denial*
  # test would be a fallback that hides the boundary rather than a slow path.
  project() {  # dir
    local dir="$1"
    mkdir -p "$dir"
    cat > "$dir/composer.json" <<EOF
{
  "name": "heavyauthz/consumer",
  "config": { "secure-http": false },
  "repositories": [
    { "type": "composer", "url": "$url" },
    { "packagist.org": false }
  ],
  "require": { "$pkg": "1.0.0" }
}
EOF
    return 0
  }

  # Composer carries a bearer credential per host, which is how a private
  # repository is configured for real.
  heavy_mark "composer-deny"
  heavy_log "composer update as the denied user — must fail"
  project "$HEAVY_WORK/composer-deny"
  set +e
  COMPOSER_CACHE_DIR="$HEAVY_WORK/composer-cache-deny" \
    COMPOSER_AUTH="{\"bearer\":{\"$host\":\"$T_DENIED\"}}" \
    bash -c "cd '$HEAVY_WORK/composer-deny' && ${composer[*]} update --no-progress" \
    >"$HEAVY_WORK/composer-deny.log" 2>&1
  local deny_rc=$?
  set -e
  [[ $deny_rc -ne 0 ]] || { tail -30 "$HEAVY_WORK/composer-deny.log" >&2; \
    heavy_fail "composer update SUCCEEDED for a caller holding no read verb"; }
  [[ -d "$HEAVY_WORK/composer-deny/vendor/$pkg" ]] \
    && heavy_fail "composer failed and installed the package anyway"
  heavy_wire_after "composer-deny" "$WIRE_403" "composer was stopped without a 403 in the transcript"

  heavy_mark "composer-allow"
  heavy_log "composer update as the reader — must succeed"
  project "$HEAVY_WORK/composer-allow"
  COMPOSER_CACHE_DIR="$HEAVY_WORK/composer-cache-allow" \
    COMPOSER_AUTH="{\"bearer\":{\"$host\":\"$T_READER\"}}" \
    bash -c "cd '$HEAVY_WORK/composer-allow' && ${composer[*]} update --no-progress" \
    >"$HEAVY_WORK/composer-allow.log" 2>&1 \
    || { tail -30 "$HEAVY_WORK/composer-allow.log" >&2; \
         heavy_fail "composer update failed for the reader — the positive control"; }
  [[ -f "$HEAVY_WORK/composer-allow/vendor/$pkg/composer.json" ]] \
    || heavy_fail "$pkg was not installed into vendor/"

  heavy_log "COMPOSER-AUTHZ-OK (refused, served)"
  return 0
}

phase_conda() {
  local mm_version="${MICROMAMBA_VERSION:-2.9.0}"
  local subdir="${CONDA_SUBDIR:-linux-64}"
  local mm_dir mm
  mm_dir="$(heavy_cached_dir "micromamba-$mm_version" \
    "https://micro.mamba.pm/api/micromamba/linux-64/$mm_version" tar.bz2)"
  mm="$mm_dir/bin/micromamba"
  [[ -x "$mm" ]] || heavy_fail "micromamba was not where the archive was expected to put it ($mm)"

  local channel="$HEAVY_TAP_BASE/proxy/$CONDA"
  local name="heavy-authz-$HEAVY_RUN"
  local file="$HEAVY_WORK/$name-1.0.0-0.tar.bz2"

  python3 tests/heavy/make_conda_package.py "$file" "$name" 1.0.0 0 "$subdir" \
    || heavy_fail "building the conda package failed"

  heavy_mark "conda-seed"
  curl -fsS -o /dev/null -X POST -H "Authorization: Bearer $T_ADMIN" \
    --data-binary @"$file" "$channel/$subdir/" || heavy_fail "publishing failed"

  # micromamba sends no Authorization header of its own, so the credential goes
  # in the channel URL's userinfo — which is also how a real conda user
  # configures a private channel.
  local host="127.0.0.1:$HEAVY_TAP_PORT"
  local deny_channel="http://authz-denied:$T_DENIED@$host/proxy/$CONDA"
  local allow_channel="http://authz-reader:$T_READER@$host/proxy/$CONDA"

  create_env() {  # suffix, channel, package
    local suffix="$1" channel_url="$2" package="$3"
    local root="$HEAVY_WORK/mamba-root-$suffix"
    mkdir -p "$root"
    MAMBA_ROOT_PREFIX="$root" "$mm" create -y --no-rc --override-channels \
      -c "$channel_url" --platform "$subdir" -n probe "$package"
    # micromamba's exit status **is** the assertion in both arms below — the
    # denial reads it out of `$?` under `set +e`, and the positive control hangs
    # a `||` off it. `return 0` here would make both pass unconditionally.
    return $?
  }

  # **`repodata.json` filters, it does not refuse** — it is a whole-registry
  # document (§4.4) and conda fetches it on every `install`, so the assertion is
  # on the *document* rather than on a status. A `403` would be the wrong answer
  # twice: it would tell an unauthorised caller the channel exists and has
  # something in it, and it would break the mirror case the filter is for.
  #
  # This is also the surface where wiring the filter found a disclosure that
  # predates RFC 0015 — `repodata.json` was built from `get_versions` with no
  # visibility check at all, so a team-visible package was named to everyone who
  # fetched the channel. This is that, asked of grants instead of visibility.
  heavy_mark "conda-document"
  heavy_log "repodata.json — the inventory the denied caller receives"
  local reader_doc denied_doc
  reader_doc="$(authz_body GET "$T_READER" "/proxy/$CONDA/$subdir/repodata.json")"
  denied_doc="$(authz_body GET "$T_DENIED" "/proxy/$CONDA/$subdir/repodata.json")"
  grep -q "$name" <<<"$reader_doc" \
    || heavy_fail "the reader's repodata does not name $name — the positive control: without \
it the assertion below passes against a channel that names nothing to anybody"
  if grep -q "$name" <<<"$denied_doc"; then
    heavy_fail "the repodata served to a caller holding no read verb names $name — conda \
fetches this document on every install, so that is the channel's inventory disclosed"
  fi

  heavy_mark "conda-deny"
  heavy_log "micromamba create as the denied user — must fail"
  set +e
  create_env deny "$deny_channel" "$name" >"$HEAVY_WORK/conda-deny.log" 2>&1
  local deny_rc=$?
  set -e
  [[ $deny_rc -ne 0 ]] || { tail -40 "$HEAVY_WORK/conda-deny.log" >&2; \
    heavy_fail "micromamba resolved the channel for a caller holding no read verb"; }

  heavy_mark "conda-allow"
  heavy_log "micromamba create as the reader — must succeed"
  create_env allow "$allow_channel" "$name" >"$HEAVY_WORK/conda-allow.log" 2>&1 \
    || { tail -40 "$HEAVY_WORK/conda-allow.log" >&2; heavy_fail "micromamba failed for the reader — the positive control"; }
  [[ -d "$HEAVY_WORK/mamba-root-allow/envs/probe" ]] || heavy_fail "no environment was created"

  heavy_log "CONDA-AUTHZ-OK (refused, served)"
  return 0
}

phase_rubygems() {
  local ruby_version="${RUBY_VERSION:-3.3.6}"
  local bundler_version="${BUNDLER_VERSION:-2.5.23}"
  local rb=()
  if ruby -v >/dev/null 2>&1; then
    :
  elif command -v mise >/dev/null 2>&1 && mise x "ruby@$ruby_version" -- ruby -v >/dev/null 2>&1; then
    rb=(mise x "ruby@$ruby_version" --)
  else
    heavy_fail "no working ruby (and no mise toolchain for ruby@$ruby_version)"
  fi
  # The status is the answer at the `gem list -i` probe below, so it is returned
  # rather than swallowed.
  ruby_run() { "${rb[@]}" "$@"; return $?; }

  heavy_log "Ruby: $(ruby_run ruby -v)"
  if ! ruby_run gem list -i bundler -v "$bundler_version" >/dev/null 2>&1; then
    ruby_run gem install bundler -v "$bundler_version" --no-document >/dev/null
  fi

  local gem_name="heavy_authz_$HEAVY_RUN"
  local source="$HEAVY_TAP_BASE/proxy/$GEMS"
  mkdir -p "$HEAVY_WORK/gems/$gem_name/lib"
  echo "module Probe; end" > "$HEAVY_WORK/gems/$gem_name/lib/$gem_name.rb"

  # **Three versions, not one.** RFC 0017's filter narrows a version index, and
  # an index with one entry narrows to either one entry or none — neither of
  # which distinguishes a filter that works from a filter that is inert. The
  # middle version is the one granted below, so the assertion is that Bundler
  # resolves to `2.0.0` while `3.0.0` exists and is newer: a resolver picking
  # what the filter left rather than what the registry holds.
  heavy_mark "gems-seed"
  local gem_version
  for gem_version in 1.0.0 2.0.0 3.0.0; do
    printf '%s\n' \
      "Gem::Specification.new do |s|" \
      "  s.name        = \"$gem_name\"" \
      "  s.version     = \"$gem_version\"" \
      "  s.summary     = \"RFC 0015 heavy authz probe\"" \
      "  s.authors     = [\"batlehub heavy tests\"]" \
      "  s.files       = [\"lib/$gem_name.rb\"]" \
      "end" > "$HEAVY_WORK/gems/$gem_name/$gem_name.gemspec"
    (cd "$HEAVY_WORK/gems/$gem_name" && ruby_run gem build "$gem_name.gemspec" >/dev/null)
    curl -fsS -o /dev/null -X POST -H "Authorization: Bearer $T_ADMIN" \
      --data-binary @"$HEAVY_WORK/gems/$gem_name/$gem_name-$gem_version.gem" \
      "$source/api/v1/gems" || heavy_fail "publishing $gem_name-$gem_version failed"
  done

  export BUNDLE_USER_HOME="$HEAVY_WORK/bundle-home"
  mkdir -p "$BUNDLE_USER_HOME"

  bundle_install() {  # suffix, credential
    local suffix="$1" credential="$2"
    local host="127.0.0.1:$HEAVY_TAP_PORT"
    local dir="$HEAVY_WORK/proj-$suffix"
    mkdir -p "$dir"
    cat > "$dir/Gemfile" <<EOF
source "http://$credential@$host/proxy/$GEMS"
gem "$gem_name"
EOF
    (cd "$dir" && "${rb[@]}" bundle "_${bundler_version}_" config set --local path "$dir/vendor" >/dev/null \
      && "${rb[@]}" bundle "_${bundler_version}_" install)
    # Bundler's exit status is what both arms assert on — see `create_env`.
    return $?
  }

  # **The compact index filters, it does not refuse.** `/versions` is a
  # whole-registry document (§4.4) — the very one RFC 0015 phase 0b measured —
  # so a caller holding no read verb gets `200` with an index naming nothing,
  # and Bundler then fails with "could not find gem" rather than with a `403`.
  # That is the correct answer twice over: a `403` would disclose that the
  # registry has something in it, and it would break the mirror case the filter
  # exists for. So the assertion is on the document.
  heavy_mark "gems-document"
  heavy_log "the compact index — the inventory the denied caller receives"
  local reader_index denied_index
  reader_index="$(authz_body GET "$T_READER" "/proxy/$GEMS/versions")"
  denied_index="$(authz_body GET "$T_DENIED" "/proxy/$GEMS/versions")"
  grep -q "$gem_name" <<<"$reader_index" \
    || heavy_fail "the reader's compact index does not name $gem_name — the positive control: \
without it the assertion below passes against an index that names nothing to anybody"
  if grep -q "$gem_name" <<<"$denied_index"; then
    heavy_fail "the compact index served to a caller holding no read verb names $gem_name — \
Bundler fetches this document on every resolve, so that is the registry's inventory disclosed"
  fi

  heavy_mark "gems-deny"
  heavy_log "bundle install as the denied user — must fail"
  set +e
  bundle_install deny "authz-denied:$T_DENIED" >"$HEAVY_WORK/gems-deny.log" 2>&1
  local deny_rc=$?
  set -e
  [[ $deny_rc -ne 0 ]] || { tail -40 "$HEAVY_WORK/gems-deny.log" >&2; \
    heavy_fail "bundle install SUCCEEDED for a caller holding no read verb"; }

  heavy_mark "gems-allow"
  heavy_log "bundle install as the reader — must succeed"
  bundle_install allow "authz-reader:$T_READER" >"$HEAVY_WORK/gems-allow.log" 2>&1 \
    || { tail -40 "$HEAVY_WORK/gems-allow.log" >&2; heavy_fail "bundle install failed for the reader — the positive control"; }
  grep -q "$gem_name" "$HEAVY_WORK/proj-allow/Gemfile.lock" \
    || heavy_fail "$gem_name missing from Gemfile.lock"

  # ── RFC 0017 — the version tier, under a real resolver ─────────────────────
  #
  # §4.4 rule 2's second half, which nothing could reach before the grants
  # editor: a caller holding `releases:list` **without** `releases:read` has a
  # read verdict decided per version, so a version-tier row is the only thing
  # that puts a version in their index. `authz-lister` is exactly that caller
  # here — `[registries.grants]` gives it the list and nothing else.
  #
  # This is the one protocol document where the filter is observable. Most
  # version documents gate on `releases:read` at the handler, so a list-only
  # caller is refused before the funnel the filter lives in; the rubygems
  # compact index authorizes *inside* the funnel, through `check_read_access`'s
  # `releases:list`.
  #
  # And it is asked of Bundler rather than of `curl` alone, because the claim is
  # not "the document has fewer lines" — it is that a real resolver, offered a
  # filtered index, resolves to what the grant left. A route test cannot tell
  # those apart.

  heavy_mark "gems-version-inert"
  heavy_log "before any version grant — §9's promise: the index is what it always was"
  local lister_index
  lister_index="$(authz_body GET "$T_LISTER" "/proxy/$GEMS/info/$gem_name")"
  local v
  for v in 1.0.0 2.0.0 3.0.0; do
    grep -q "^$v " <<<"$lister_index" || heavy_fail "before any version-tier row exists the \
list-only caller's compact index must name every version, and $v is missing — RFC 0017 §9 \
promises the filter is inert until an operator writes the first grant, and an index that is \
already short makes every assertion below pass for the wrong reason"
  done

  heavy_mark "gems-version-grant"
  heavy_log "a version-tier grant on 2.0.0 — written through the editor RFC 0017 adds"
  # `source:read` alongside `releases:read`: the index decides what Bundler
  # *resolves*, the download gate decides what it may *fetch*, and a grant
  # carrying only the first would filter the index correctly and then fail the
  # install for a reason that has nothing to do with the filter.
  local grant_status
  grant_status="$(authz_status PUT "$T_ADMIN" "/api/v1/admin/registries/$GEMS/grants" \
    -H "$HDR_JSON" \
    --data "{\"package\":\"$gem_name\",\"version\":\"2.0.0\",\"subject\":\"user:authz-lister\",\"actions\":[\"releases:read\",\"source:read\"]}")"
  [[ "$grant_status" == "200" ]] || heavy_fail \
    "writing the version-tier grant returned $grant_status — the whole RFC is that this PUT exists"

  lister_index="$(authz_body GET "$T_LISTER" "/proxy/$GEMS/info/$gem_name")"
  grep -q "^2.0.0 " <<<"$lister_index" || heavy_fail \
    "the granted version is absent from the list-only caller's index — the positive control, \
without which 'names neither of the others' passes against an empty document"
  for v in 1.0.0 3.0.0; do
    if grep -q "^$v " <<<"$lister_index"; then
      heavy_fail "the compact index served to a caller granted the read on 2.0.0 alone names \
$v — that is the existence and the number of a release they may not read, which is the \
disclosure §2.3 says the filter has to ship with the writer to prevent"
    fi
  done

  # Grants only widen (§4.3). The reader holds `releases:read` on the registry,
  # so it holds it on every version beneath, and a row written for someone else
  # cannot subtract from that. This is the assertion the RFC's own before/after
  # example got backwards.
  local reader_info
  reader_info="$(authz_body GET "$T_READER" "/proxy/$GEMS/info/$gem_name")"
  for v in 1.0.0 2.0.0 3.0.0; do
    grep -q "^$v " <<<"$reader_info" || heavy_fail "the reader's index lost $v — a version-tier \
row written for another subject narrowed what this one sees, which §4.3 forbids and a union \
cannot express"
  done

  # §11.6's oracle, asked about the row that was just written, on the server
  # that just accepted it.
  authz_oracle releases:read "$GEMS" "user:authz-lister" allow "$gem_name" "2.0.0"
  authz_oracle releases:read "$GEMS" "user:authz-lister" deny "$gem_name" "3.0.0"

  heavy_mark "gems-version-resolve"
  heavy_log "bundle install as the lister — must resolve to 2.0.0, not to the newest"
  bundle_install lister "authz-lister:$T_LISTER" >"$HEAVY_WORK/gems-lister.log" 2>&1 \
    || { tail -40 "$HEAVY_WORK/gems-lister.log" >&2; heavy_fail "bundle install failed for the \
caller granted 2.0.0 — the filter left them a resolvable index and the install still did not work"; }
  grep -qE "^    $gem_name \(2\.0\.0\)" "$HEAVY_WORK/proj-lister/Gemfile.lock" || {
    cat "$HEAVY_WORK/proj-lister/Gemfile.lock" >&2
    heavy_fail "Bundler did not pin 2.0.0. The Gemfile names no version, so an unfiltered index \
resolves to 3.0.0 — pinning 2.0.0 is the filter changing what a real resolver picks, and it is \
the only assertion here that a route test could not have made"
  }

  heavy_log "RUBYGEMS-AUTHZ-OK (refused, served, filtered to the granted version)"
  return 0
}

phase_openvsx() {
  heavy_need npx "nodejs"
  local ovsx_version="${OVSX_VERSION:-1.1.1}"
  local ext_name="probe$HEAVY_RUN"
  local registry_url="$HEAVY_TAP_BASE/proxy/$VSX"

  # `ovsx get` sends **no credential** — not a header, not the `?token=` query
  # parameter it uses on publish — which the first version of this phase
  # discovered the hard way: the denied arm and the reader arm were both
  # anonymous, both got `403`, and the denial "passed" while proving nothing.
  # The positive control is what caught it, which is the whole argument for
  # having one.
  #
  # So this phase splits along what the client can actually carry:
  #
  #   publish  ovsx sends `--pat`, so the refusal is attributable — and the one
  #            thing a publish can be refused for on a local registry is a seal
  #            (§10 rule 5 hands `releases:publish` to every `role:user`, and
  #            grants only widen). Same client, same command, one namespace
  #            apart.
  #   read     asserted with curl, because no identity can be attached to
  #            `ovsx get`. The VSIX download asks for `source:read` rather than
  #            `releases:read`, which is why a reader granted only the latter
  #            still cannot fetch an extension.

  build_vsix() {  # publisher -> echoes the vsix path
    local publisher="$1"
    local dir="$HEAVY_WORK/ext-$publisher"
    local vsce_version="${VSCE_VERSION:-3.9.2}"
    mkdir -p "$dir"
    cat > "$dir/package.json" <<EOF
{ "name": "$ext_name", "displayName": "RFC 0015 heavy authz probe",
  "description": "Published here and refused to a caller who may not read it.",
  "publisher": "$publisher", "version": "1.0.0", "license": "MIT",
  "engines": { "vscode": "^1.75.0" }, "categories": ["Other"],
  "main": "./extension.js", "activationEvents": ["onStartupFinished"], "contributes": {} }
EOF
    printf 'function activate() {}\nmodule.exports = { activate };\n' > "$dir/extension.js"
    echo "# probe" > "$dir/README.md"
    echo "MIT" > "$dir/LICENSE"
    local vsix="$HEAVY_WORK/$publisher-$ext_name-1.0.0.vsix"
    (cd "$dir" && npx --yes "@vscode/vsce@$vsce_version" package \
      --no-dependencies --allow-missing-repository --skip-license -o "$vsix") \
      >>"$HEAVY_WORK/vsce.log" 2>&1 \
      || { tail -30 "$HEAVY_WORK/vsce.log" >&2; heavy_fail "vsce package failed for $publisher"; }
    echo "$vsix"
    return 0
  }

  # ── the client, publishing where it may ──
  heavy_mark "vsx-seed"
  heavy_log "ovsx publish into an open namespace — the positive control"
  local open_vsix
  open_vsix="$(build_vsix heavyorg)"
  npx --yes "ovsx@$ovsx_version" publish "$open_vsix" \
    --registryUrl "$registry_url" --pat "$T_ADMIN" >"$HEAVY_WORK/vsx-publish.log" 2>&1 \
    || { cat "$HEAVY_WORK/vsx-publish.log" >&2; heavy_fail "ovsx publish failed — the positive control"; }
  heavy_wire_after "vsx-seed" "POST /proxy/$VSX/api/-/publish" "ovsx did not reach the publish endpoint"

  # ── the client, publishing where it may not ──
  heavy_mark "vsx-sealed"
  heavy_log "ovsx publish into the sealed namespace — must fail, for everybody"
  local sealed_vsix
  sealed_vsix="$(build_vsix sealed)"
  set +e
  npx --yes "ovsx@$ovsx_version" publish "$sealed_vsix" \
    --registryUrl "$registry_url" --pat "$T_ADMIN" >"$HEAVY_WORK/vsx-sealed.log" 2>&1
  local sealed_rc=$?
  set -e
  [[ $sealed_rc -ne 0 ]] || { cat "$HEAVY_WORK/vsx-sealed.log" >&2; heavy_fail \
    "ovsx published into a sealed namespace — a seal that role:admin walks through is not a seal"; }
  heavy_wire_after "vsx-sealed" "$WIRE_403" \
    "ovsx was stopped, but no 403 followed the sealed publish — so it failed for some other reason"

  # ── the read boundary, over curl for the reason above ──
  heavy_mark "vsx-read"
  heavy_log "the VSIX download — source:read, refused and served"
  local vsix_path="/proxy/$VSX/heavyorg.$ext_name/1.0.0/vsix"
  local denied_status reader_status
  denied_status="$(authz_status GET "$T_DENIED" "$vsix_path")"
  reader_status="$(authz_status GET "$T_READER" "$vsix_path")"
  [[ "$denied_status" == "403" ]] || heavy_fail \
    "the denied caller got $denied_status for the VSIX, expected 403"
  [[ "$reader_status" != "403" && "$reader_status" != "401" ]] || heavy_fail \
    "the reader got $reader_status for the VSIX — the positive control"

  heavy_log "OPENVSX-AUTHZ-OK (a seal the client meets, and the read boundary beside it)"
  return 0
}

phase_terraform() {
  local terraform_version="${TERRAFORM_VERSION:-1.8.5}"
  heavy_runner_for terraform "terraform@$terraform_version"
  local tf=("${HEAVY_RUNNER[@]}" terraform)

  local provider="${TF_PROVIDER:-hashicorp/null}"
  local provider_version="${TF_PROVIDER_VERSION:-3.2.2}"
  local ns="${provider%%/*}" ptype="${provider##*/}"
  local tf_host="localhost:$HEAVY_TAP_PORT"

  export SSL_CERT_FILE="$HEAVY_AUTHZ_CERT"
  heavy_log "terraform $("${tf[@]}" version | head -1)"

  local project="$HEAVY_WORK/project"
  mkdir -p "$project"
  cat > "$project/main.tf" <<EOF
terraform {
  required_providers {
    probe = {
      source  = "$tf_host/$ns/$ptype"
      version = "$provider_version"
    }
  }
}
EOF
  export TF_PLUGIN_CACHE_DIR="$HEAVY_WORK/plugin-cache"
  mkdir -p "$TF_PLUGIN_CACHE_DIR"
  export TF_IN_AUTOMATION=1 CHECKPOINT_DISABLE=1

  tf_credentials() {  # token, suffix
    local token="$1" suffix="$2"
    export TF_CLI_CONFIG_FILE="$HEAVY_WORK/terraformrc-$suffix"
    cat > "$TF_CLI_CONFIG_FILE" <<EOF
plugin_cache_dir = "$TF_PLUGIN_CACHE_DIR"
disable_checkpoint = true
credentials "$tf_host" {
  token = "$token"
}
EOF
    return 0
  }

  # Terraform sends its credential to the two metadata documents and fetches the
  # provider archive, its `SHA256SUMS` and the `.sig` with **no** Authorization
  # header (RFC 0009 §12.3 — measured against 1.8.5, not read). That is what
  # RFC 0012's signed URLs exist for, and this registry has them on
  # (`signed_downloads = true`), so the whole install is assertable against a
  # registry that grants anonymous **nothing**:
  #
  #   deny   the credential reaches the versions document, is refused, and
  #          `init` stops there.
  #   allow  the same `init`, one token different, runs to completion — the
  #          documents on the credential, the archive on a minted signature.
  #
  # An earlier version of this phase asserted only the versions document and
  # said a whole `init` was "not reproducible here without making the registry
  # anonymous-readable, which would delete the boundary being tested". That was
  # wrong, and wrong in the direction that matters: it left the one mechanism
  # that closes this hole — seven landed phases of it — covered by no heavy test
  # at all.
  heavy_mark "tf-deny"
  heavy_log "terraform init as the denied user — must fail at the versions document"
  tf_credentials "$T_DENIED" denied
  set +e
  (cd "$project" && rm -rf .terraform && "${tf[@]}" init -no-color) >"$HEAVY_WORK/tf-deny.log" 2>&1
  local deny_rc=$?
  set -e
  [[ $deny_rc -ne 0 ]] || { cat "$HEAVY_WORK/tf-deny.log" >&2; \
    heavy_fail "terraform init SUCCEEDED for a caller holding no read verb"; }
  # The path is the **client's**, with no `/proxy/{registry}` prefix: this
  # registry is host-routed, and the middleware rewrites the request after the
  # tap has already recorded what Terraform sent.
  heavy_wire_after "tf-deny" "GET /v1/providers/$ns/$ptype/versions -> 403" \
    "terraform was stopped, but not by a 403 on the versions document — so the denial is not \
the grant hierarchy's and this phase proves nothing"

  heavy_mark "tf-allow"
  heavy_log "terraform init as the reader — the whole install, on a registry closed to anonymous"
  tf_credentials "$T_READER" reader
  set +e
  (cd "$project" && rm -rf .terraform && "${tf[@]}" init -no-color) >"$HEAVY_WORK/tf-allow.log" 2>&1
  local allow_rc=$?
  set -e
  if [[ $allow_rc -ne 0 ]]; then
    cat "$HEAVY_WORK/tf-allow.log" >&2
    heavy_fail "terraform init failed for the reader — the positive control. On a registry with \
signed_downloads = true and no anonymous grant this is RFC 0012's whole claim, so a failure here \
is either the signature not being minted into the download document or not being accepted on the \
artifact routes."
  fi
  grep -q "Terraform has been successfully initialized" "$HEAVY_WORK/tf-allow.log" \
    || heavy_fail "terraform init reported no success line"
  heavy_wire_after "tf-allow" "GET /v1/providers/$ns/$ptype/versions -> 200" \
    "the reader was refused the versions document"

  # ── the signature is what did it, not an anonymous grant ───────────────────
  #
  # The distinction this phase would otherwise be unable to make. A minted URL
  # carries `bh_sig`, and the archive request that follows it carries no
  # credential — so if the same URL answers with the signature stripped, the
  # registry is open and the signing is decorative.
  heavy_mark "tf-signature"
  local archive_line signed_url stripped_status
  archive_line="$(awk '/### tf-allow/{seen=1; next} seen && /bh_sig=/ && / -> 200/ {print; exit}' "$HEAVY_LOG")"
  [[ -n "$archive_line" ]] || heavy_fail \
    "no request carrying bh_sig answered 200 during the install — the download document was \
served unsigned, so this registry is not exercising RFC 0012 at all"
  heavy_log "signed request observed: ${archive_line% -> *}"

  # Replay one signed path with the signature removed, unauthenticated. The
  # capability is the only thing that opened it, so without it this must refuse.
  signed_url="$(sed -E 's/^[A-Z]+ //; s/ -> .*$//' <<<"$archive_line")"
  stripped_status="$(authz_status GET "$T_ANON" "${signed_url%%\?*}")"
  [[ "$stripped_status" == "403" ]] || heavy_fail \
    "the same artifact URL answered $stripped_status with the bh_sig stripped and no credential, \
expected 403 — the install above was served by an open registry rather than by a signature, and \
every signed-URL assertion in this phase is vacuous"

  # A minted document is a bearer capability. Served with a cacheable answer, a
  # shared cache hands one caller's capability to the next.
  local cache_header
  cache_header="$(curl -s -D - -o /dev/null --cacert "$HEAVY_AUTHZ_CERT" \
    -H "Authorization: Bearer $T_READER" \
    "$HEAVY_TAP_BASE/v1/providers/$ns/$ptype/$provider_version/download/linux/amd64" \
    | grep -i '^cache-control' || true)"
  grep -qi "no-store" <<<"$cache_header" || heavy_fail \
    "the signed download document came back with Cache-Control '${cache_header:-<absent>}' — it \
carries a minted capability and must be no-store, or a shared cache replays one caller's \
signature to the next"

  heavy_log "TERRAFORM-AUTHZ-OK (refused, installed end-to-end on a closed registry, and the signature proven load-bearing)"
  return 0
}

# ═════════════════════════════════════════════════════════════════════════════
# Signed URLs — expiry, binding, and secret rotation (RFC 0012)
# ═════════════════════════════════════════════════════════════════════════════
#
# The `terraform` target proves a minted capability *works*: a closed registry
# serves a whole `terraform init`, and the same URL with `bh_sig` stripped is
# refused. What it cannot prove is the three properties that make a capability
# safe to hand out, because each needs a clock or a key change that a single
# `terraform init` cannot produce:
#
#   expiry     a URL replayed after `ttl_seconds` must stop working. Without
#              this, `ttl_seconds` is a number in a config file that nothing
#              enforces, and a leaked URL is a permanent grant.
#   binding    a capability minted for one artifact must not open another. The
#              payload names `art`, and this is the assertion that the name is
#              load-bearing rather than decorative.
#   rotation   `previous_secrets` is documented as "verified against but never
#              minted with, so a secret can be rotated without a flag day". Both
#              halves need asserting: the old URL keeps working during the
#              overlap, and it stops the moment the old secret is retired. An
#              operator who rotates and finds every in-flight install broken has
#              a worse outage than the one they were avoiding.
#
# It needs neither a client nor TLS — the capability is a URL, and `curl` is the
# honest way to replay one. What it does need is the ability to change the
# server's key mid-run, which `[server.signed_urls]` supports because
# `build_hot_config` rebuilds `signed_url` on every reload. So this target runs
# against a **generated** copy of the config whose signing block it rewrites,
# and drives the changes through `POST /api/v1/admin/config/reload`.

SECRET_A="heavy-authz-rotation-secret-alpha-000000000000000"
SECRET_B="heavy-authz-rotation-secret-bravo-111111111111111"

# signing_config <path> <ttl> <secret> [previous-secret…] — write a config whose
# `[server.signed_urls]` block says exactly this.
#
# A literal secret rather than `${HEAVY_SIGNING_SECRET}`: the loader expands
# `${VAR}` before parsing, so a placeholder would make every stage read the same
# key and the rotation stages would silently test nothing.
signing_config() {
  local path="$1" ttl="$2" secret="$3"
  shift 3
  python3 - "$path" "$ttl" "$secret" "$@" <<'PY'
import sys

path, ttl, secret, previous = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4:]
lines = open(path).read().split("\n")
start = lines.index("[server.signed_urls]")
end = start + 1
while end < len(lines) and not lines[end].startswith("["):
    end += 1

block = ["[server.signed_urls]", f'secret = "{secret}"', f"ttl_seconds = {ttl}"]
if previous:
    inner = ", ".join(f'"{p}"' for p in previous)
    block.append(f"previous_secrets = [{inner}]")
block.append("")

open(path, "w").write("\n".join(lines[:start] + block + lines[end:]))
PY
  return 0
}

# signing_reload <what> <ttl> <secret> [previous-secret…] — put that signing
# block in force.
#
# Three things this does not do, each because it was tried:
#
# **It does not edit the file the server watches.** The watcher polls every two
# seconds and *stages* a pending rather than applying one, so editing that file
# races `load_pending_from_content`'s byte-identical dedup: whichever of the two
# loads the content first marks it seen, the other returns `pending_created:
# false`, and the apply that follows answers "no pending reload". Measured, not
# reasoned — and one `open(path, "w")` fires the watcher up to five times, which
# is exactly its rate limit, so it then disables itself mid-run.
#
# **It does not use `POST /config/reload`.** That endpoint consumes a snapshot
# the watcher stages; with nothing staged it answers "no pending reload".
#
# **It does not trust a 2xx.** `pending_created` is asserted, because the dedup
# path returns success with nothing to apply — a reload that quietly did nothing
# would leave every assertion after it describing the previous stage's key.
#
# The posted content goes through the same loader, so `${VAR}` is expanded from
# the *server's* environment exactly as at startup, which is why the block
# carries a literal secret and everything else stays a placeholder.
signing_reload() {
  local what="$1"
  shift
  local staged created status body

  signing_config "$HEAVY_AUTHZ_SIGNING_STAGE" "$@"
  python3 -c '
import json, sys
print(json.dumps({"content": open(sys.argv[1]).read()}))
' "$HEAVY_AUTHZ_SIGNING_STAGE" > "$HEAVY_WORK/from-content.json"

  staged="$(authz_body POST "$T_ADMIN" "/api/v1/admin/config/from-content" \
    -H "$HDR_JSON" --data-binary @"$HEAVY_WORK/from-content.json")"
  created="$(python3 -c '
import json, sys
try:
    print(json.load(sys.stdin).get("pending_created"))
except Exception:
    print("unparseable")
' <<<"$staged")"
  [[ "$created" == "True" ]] || heavy_fail \
    "staging the config for '$what' created no pending (pending_created=$created): $staged"

  status="$(authz_status POST "$T_ADMIN" "/api/v1/admin/config/pending/apply")"
  if [[ "$status" != 2* ]]; then
    body="$(authz_body POST "$T_ADMIN" "/api/v1/admin/config/pending/apply")"
    heavy_fail "applying the config for '$what' answered $status: $body"
  fi
  heavy_log "reloaded: $what"
  return 0
}

# signing_exp_in <signed-path> — seconds from now until the capability expires.
#
# Decoded from the token rather than inferred from the config, because those are
# two different claims: the config says what the server was told, and `exp` says
# what it actually minted. A phase that asserts expiry by sleeping and replaying
# cannot tell "the TTL is not enforced" from "the TTL was never applied", and
# those have opposite fixes.
signing_exp_in() {
  local signed_path="$1"
  python3 -c '
import base64, json, sys, time, urllib.parse

query = urllib.parse.urlsplit(sys.argv[1]).query
token = urllib.parse.parse_qs(query).get("bh_sig", [""])[0]
parts = token.split(".")
if len(parts) < 2:
    print("unparseable")
    raise SystemExit
raw = parts[1]
raw += "=" * (-len(raw) % 4)
try:
    print(int(json.loads(base64.urlsafe_b64decode(raw))["exp"] - time.time()))
except Exception:
    print("unparseable")
' "$signed_path"
  return 0
}

# signing_path <absolute-url> — the path+query, for the assertion helpers.
signing_path() {
  local url="$1"
  python3 -c '
import sys, urllib.parse
u = urllib.parse.urlsplit(sys.argv[1])
print(u.path + ("?" + u.query if u.query else ""))
' "$url"
  return 0
}

# signing_mint <field> — echo the signed path the download document carries.
#
# Minted by asking the server for the document as the reader, never by building
# one here: a suite that signs its own URLs is testing its copy of the algorithm
# rather than the server's, and the two can agree while both are wrong.
signing_mint() {
  local field="$1" doc url
  doc="$(authz_body GET "$T_READER" \
    "/proxy/$TFREG/v1/providers/$TF_NS/$TF_TYPE/$TF_VERSION/download/linux/amd64")"
  url="$(python3 -c '
import json, sys
try:
    print(json.load(sys.stdin).get(sys.argv[1], ""))
except Exception:
    print("")
' "$field" <<<"$doc")"
  [[ -n "$url" ]] || heavy_fail "the download document carried no $field — nothing to sign or replay"
  case "$url" in
    *bh_sig=*) ;;
    *) heavy_fail "the $field in the download document carries no bh_sig, so this registry is \
serving an unsigned document and every assertion in this phase would be vacuous" ;;
  esac
  signing_path "$url"
  return 0
}

phase_signing() {
  TF_NS="${TF_PROVIDER_NS:-hashicorp}"
  TF_TYPE="${TF_PROVIDER_TYPE:-null}"
  TF_VERSION="${TF_PROVIDER_VERSION:-3.2.2}"

  local signed replayed

  # ── 1. A minted capability opens exactly what it names ─────────────────────
  heavy_mark "sig-binding"
  heavy_log "a capability is bound to its artifact"

  local shasums_path sig_path
  shasums_path="$(signing_mint shasums_url)"
  sig_path="$(signing_mint shasums_signature_url)"

  # The positive control first: the capability works at all, anonymously.
  replayed="$(authz_status GET "$T_ANON" "$shasums_path")"
  [[ "$replayed" == "200" ]] || heavy_fail \
    "a freshly minted shasums capability answered $replayed to an anonymous caller, expected \
200 — the positive control, without which every refusal below is indistinguishable from a \
broken registry"

  # Now the binding. Take the signature minted for `shasums` and present it on
  # the `shasums.sig` route: same registry, same provider, same version, same
  # caller, different artifact. The payload names `art`, and this is where that
  # name earns its place.
  local swapped="${sig_path%%\?*}?${shasums_path#*\?}"
  replayed="$(authz_status GET "$T_ANON" "$swapped")"
  [[ "$replayed" == "403" ]] || heavy_fail \
    "a capability minted for 'shasums' answered $replayed on the 'shasums.sig' route, expected \
403 — one minted URL would then open every artifact of the version, which is a wider grant than \
the document that carried it"

  # ── 2. Expiry ──────────────────────────────────────────────────────────────
  #
  # Two seconds, applied by reload. Short enough that the run does not wait on
  # it, long enough that a slow round trip does not expire the URL before the
  # positive control below can prove it started out valid.
  heavy_mark "sig-expiry"
  heavy_log "a capability stops working when its ttl lapses"

  signing_reload "ttl_seconds = 2" 2 "$SECRET_A"

  signed="$(signing_mint shasums_url)"

  # What the server minted, not what the config said. These are different
  # claims, and telling them apart is the difference between "expiry is not
  # enforced" and "the reload never reached the signer".
  local exp_in
  exp_in="$(signing_exp_in "$signed")"
  [[ "$exp_in" =~ ^-?[0-9]+$ ]] || heavy_fail \
    "could not read exp out of the minted capability ($exp_in) — the token layout changed and \
this phase is no longer checking what it says it checks"
  if [[ "$exp_in" -gt 10 ]]; then
    heavy_fail "the capability minted after ttl_seconds = 2 expires in ${exp_in}s, so the signer \
is still using the previous TTL — the reload reached the config and not the minting path"
  fi

  replayed="$(authz_status GET "$T_ANON" "$signed")"
  [[ "$replayed" == "200" ]] || heavy_fail \
    "a capability minted under ttl_seconds = 2 answered $replayed immediately, expected 200 — \
it was born expired, so the expiry assertion below would pass for the wrong reason"

  # **A token is not dead at `exp`.** `verify_at` refuses at `exp + skew`, where
  # the skew is a deliberate backward-clock allowance: "a runner whose clock is a
  # minute behind the minter must not fail an install; forward skew is not
  # tolerated, because that direction only ever extends a credential's life."
  #
  # Read out of the source rather than written here, for the same reason the
  # verb list is: a constant copied into a test is a constant that drifts, and
  # this one drifting silently would turn the assertion below into a sleep.
  local skew
  skew="$(grep -oE 'CLOCK_SKEW_SECS: i64 = [0-9]+' crates/core/src/services/signed_url.rs \
    | grep -oE '[0-9]+$')"
  [[ "$skew" =~ ^[0-9]+$ ]] || heavy_fail \
    "could not read CLOCK_SKEW_SECS out of crates/core/src/services/signed_url.rs — the wait \
below would be a guess, and a guess that is too short reports a working expiry as broken"

  # Just past `exp`, still inside the allowance. Pinning this edge is what makes
  # the next assertion mean "expiry is enforced" rather than "something refused
  # eventually": without it, a token that died at `exp` and a token that died at
  # `exp + skew` are indistinguishable, and only one of them is the documented
  # behaviour.
  sleep $(( exp_in > 0 ? exp_in + 2 : 2 ))
  replayed="$(authz_status GET "$T_ANON" "$signed")"
  [[ "$replayed" == "200" ]] || heavy_fail \
    "a capability replayed just past exp but inside the ${skew}s clock-skew allowance answered \
$replayed, expected 200 — a runner whose clock is a minute behind the minter would fail every \
install, which is what the allowance exists to prevent"

  # And now past the allowance.
  heavy_log "waiting out the ${skew}s clock-skew allowance before replaying"
  sleep $(( skew + 3 ))
  replayed="$(authz_status GET "$T_ANON" "$signed")"
  [[ "$replayed" == "403" ]] || heavy_fail \
    "a capability replayed ${skew}s past its expiry answered $replayed, expected 403 — \
ttl_seconds is then a number nothing enforces, and a leaked URL is a permanent anonymous grant \
on a registry that grants anonymous nothing"

  # ── 3. Rotation ────────────────────────────────────────────────────────────
  #
  # The overlap window is the whole feature: an operator who rotates and finds
  # every in-flight install broken has caused a worse outage than the one they
  # were avoiding. Both halves are asserted, because only one of them failing is
  # the likely bug in either direction.
  heavy_mark "sig-rotation"
  heavy_log "secret rotation — the overlap window, and the end of it"

  signing_reload "secret A, ttl 3600" 3600 "$SECRET_A"
  local minted_under_a
  minted_under_a="$(signing_mint shasums_url)"
  replayed="$(authz_status GET "$T_ANON" "$minted_under_a")"
  [[ "$replayed" == "200" ]] || heavy_fail \
    "the URL minted under secret A answered $replayed before any rotation, expected 200"

  signing_reload "secret B, previous_secrets = [A]" 3600 "$SECRET_B" "$SECRET_A"

  replayed="$(authz_status GET "$T_ANON" "$minted_under_a")"
  [[ "$replayed" == "200" ]] || heavy_fail \
    "a URL minted under the old secret answered $replayed during the overlap window, expected \
200 — previous_secrets is documented as verified against, and an operator rotating a key would \
break every install already in flight"

  # …and the new secret mints too, so the overlap is an overlap rather than a
  # server still signing with the retired key.
  local minted_under_b
  minted_under_b="$(signing_mint shasums_url)"
  [[ "$minted_under_b" != "$minted_under_a" ]] || heavy_fail \
    "the URL minted after rotation is byte-identical to the one minted before it, so the new \
secret is not being used to sign — previous_secrets is 'verified against but never minted with'"
  replayed="$(authz_status GET "$T_ANON" "$minted_under_b")"
  [[ "$replayed" == "200" ]] || heavy_fail \
    "a URL minted under the new secret answered $replayed, expected 200"

  signing_reload "secret B alone — rotation complete" 3600 "$SECRET_B"

  replayed="$(authz_status GET "$T_ANON" "$minted_under_a")"
  [[ "$replayed" == "403" ]] || heavy_fail \
    "a URL minted under the retired secret answered $replayed after it was removed from \
previous_secrets, expected 403 — retiring a key would then not retire the capabilities it \
signed, which is the only reason to rotate one"
  replayed="$(authz_status GET "$T_ANON" "$minted_under_b")"
  [[ "$replayed" == "200" ]] || heavy_fail \
    "the URL minted under the current secret answered $replayed after the retirement, expected \
200 — rotation invalidated the wrong generation"

  heavy_log "SIGNING-AUTHZ-OK (bound to its artifact, expires, and rotates in both directions)"
  return 0
}

# ── Vocabulary coverage ──────────────────────────────────────────────────────

# The verb list comes out of the enum rather than being copied here, so a verb
# added tomorrow fails this run instead of silently going unexercised. Same
# shape as §11.5's dead-end test, asked of the wire.
authz_check_vocabulary_covered() {
  local -a all missing=()
  mapfile -t all < <(grep -oE '=> "[a-z][a-z:-]*:[a-z][a-z:-]*"' \
    crates/core/src/entities/permission.rs | sed 's/.*"\(.*\)"/\1/' | sort -u)
  [[ ${#all[@]} -ge 25 ]] || heavy_fail \
    "read ${#all[@]} verbs out of permission.rs, which cannot be right — the scan has \
drifted from the file and this check is no longer checking anything"

  local verb
  for verb in "${all[@]}"; do
    case "$verb" in
      # Requested by no route, deliberately and with the reason recorded in
      # `crates/web/tests/vocabulary_dead_ends.rs`: dist-tags here are *derived*
      # from the published version set so RFC 0006's block-repair can move
      # `latest`, and storing them has no good answer when the tagged version is
      # withdrawn. There is nothing to drive, so there is nothing to assert.
      npm:dist-tags:write) continue ;;
      # Visibility verbs, not refusal verbs (RFC 0018 §4.2). Neither ever
      # produces a `403`: `quarantine:read` chooses between a held version's
      # refusal and a plain `404` — that is §4.4 rule 2, a held version must not
      # be enumerable — and `findings:read` chooses between a reason-only body
      # and one that also carries the findings. The `authz_denied` /
      # `authz_allowed` pair this suite is built on asserts exactly `403` and
      # not-`403`, so it cannot express either without asserting the very
      # conflation (`404` as refusal) the pair exists to rule out.
      #
      # They are covered on the wire, elsewhere: `tests/heavy/quarantine.sh`
      # drives a real hold through npm and asserts the refusal body, the
      # `X-BatleHub-Reason` header and the findings `batlehub why` prints, and
      # `crates/web/tests/security_registry.rs` asserts all three visibility
      # tiers against the verdict endpoint. Both need a scanner verdict to
      # exist, which is why they are not here: this suite deliberately runs
      # without a security profile.
      quarantine:read | findings:read) continue ;;
      # Every other verb falls through to the coverage check below, which is the
      # point: a verb added tomorrow is not in this list, so it is required to
      # have been exercised.
      *) ;;
    esac
    if [[ -z "${AUTHZ_VERB_SEEN[$verb]:-}" ]]; then
      missing+=("$verb")
    fi
  done

  if [[ ${#missing[@]} -gt 0 ]]; then
    heavy_fail "the vocabulary is not covered — ${#missing[@]} verb(s) exercised by nothing \
in this run: ${missing[*]}. Either add the pair, or add it to the exception list above with \
the reason."
  fi
  heavy_log "vocabulary covered: ${#all[@]} verbs in the enum, three deliberate exceptions"
  return 0
}

# ── Run ──────────────────────────────────────────────────────────────────────

# The `signing` target rewrites its server's signing block mid-run, so it gets a
# copy: the checked-in config is not a scratch file, and a run that edited it
# would leave the tree dirty and the next run reading whichever stage the last
# one died in.
if [[ "$TARGET" == "signing" ]]; then
  HEAVY_AUTHZ_SIGNING_CONFIG="$HEAVY_WORK/config.signing.toml"
  # The staging copy is a *different* file, and that is the point — see
  # `signing_reload`. `pending/apply` persists what it applied to the watched
  # path, which fires the watcher; nothing here depends on the watcher, so that
  # is harmless.
  HEAVY_AUTHZ_SIGNING_STAGE="$HEAVY_WORK/config.stage.toml"
  cp tests/heavy/config.authz.toml "$HEAVY_AUTHZ_SIGNING_CONFIG"
  cp tests/heavy/config.authz.toml "$HEAVY_AUTHZ_SIGNING_STAGE"
  signing_config "$HEAVY_AUTHZ_SIGNING_CONFIG" 300 "$SECRET_A"
  heavy_start_server "$HEAVY_AUTHZ_SIGNING_CONFIG"
else
  heavy_start_server tests/heavy/config.authz.toml
fi

if [[ "$TARGET" == "terraform" ]]; then
  HEAVY_AUTHZ_CERT="$(heavy_self_signed "$HEAVY_TAP_HOST")"
  heavy_start_tap "$HEAVY_AUTHZ_CERT" "$HEAVY_WORK/tls-key.pem"
else
  heavy_start_tap
fi

case "$TARGET" in
  matrix)
    phase_matrix
    authz_check_vocabulary_covered
    heavy_done "AUTHZ-HEAVY-MATRIX-OK"
    ;;
  signing)   phase_signing;   heavy_done "AUTHZ-HEAVY-SIGNING-OK" ;;
  npm)       phase_npm;       heavy_done "AUTHZ-HEAVY-NPM-OK" ;;
  pypi)      phase_pypi;      heavy_done "AUTHZ-HEAVY-PYPI-OK" ;;
  nuget)     phase_nuget;     heavy_done "AUTHZ-HEAVY-NUGET-OK" ;;
  conda)     phase_conda;     heavy_done "AUTHZ-HEAVY-CONDA-OK" ;;
  openvsx)   phase_openvsx;   heavy_done "AUTHZ-HEAVY-OPENVSX-OK" ;;
  rubygems)  phase_rubygems;  heavy_done "AUTHZ-HEAVY-RUBYGEMS-OK" ;;
  terraform) phase_terraform; heavy_done "AUTHZ-HEAVY-TERRAFORM-OK" ;;
  composer)  phase_composer;  heavy_done "AUTHZ-HEAVY-COMPOSER-OK" ;;
  *)
    heavy_fail "unknown target '$TARGET' — one of: matrix signing npm pypi nuget composer conda openvsx rubygems terraform"
    ;;
esac
