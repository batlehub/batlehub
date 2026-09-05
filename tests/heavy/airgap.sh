#!/usr/bin/env bash
# Heavy air-gap suite — npm, pip and mise against a disconnected BatleHub
# that holds one version of what they ask for, before and after RFC
# 0008-bis.
#
# RFC 0008 §14.8 measured one client (mise) and found that a lock resolves
# nothing and a version string resolves through a *listing*, which is the one
# thing a bundle does not carry. RFC 0008-bis phase 0 made that a
# measurement on all three clients (its §2); phase 1 answers the listing
# from what the instance holds. This suite is both halves on one
# disconnected instance, every row read off the wire (the tap sits between
# the client and the disconnected instance) and off the client's own output.
#
# With `synthesise_listings = false` — RFC 0008's refusal, kept as the
# phase-0 measurement:
#
#   1. `npm install left-pad@1.3.0` from a clean cache: the packument is
#      asked for first, it is a 503, and npm never reaches the tarball it
#      could have had.
#   2. `npm ci` from a lockfile naming the tarball: the tarball, and nothing
#      else — the install completes with no route off the host.
#   3. `pip install six==1.17.0`: the simple page first, a 503, retried, and
#      the wheel never asked for.
#   4. `pip install` of a PEP 508 direct reference to the held wheel: the
#      wheel, and nothing else.
#   5. `mise install github:cli/cli@2.60.0` with no lock: the release *by
#      tag* first, then the listing, both 503 and both retried, and the
#      install stops there (the locked install is `mise.sh` §4's).
#
# Then the miss log is purged and `synthesise_listings` flipped to `true`
# by a hot reload, and the same commands run again (0008-bis §10):
#
#   6. `npm install left-pad@1.3.0` completes: the packument answers 200
#      with `X-BatleHub-Listing: synthesised`, then the tarball 200.
#   7. `npm install left-pad@1.2.0` — a version the instance does not hold
#      — fails in npm with ETARGET off the same synthesised packument, and
#      never asks for a tarball: the listing named only what is held.
#   8. `pip install six==1.17.0` completes: the JSON simple page 200,
#      synthesised, then the wheel 200.
#   9. `mise install github:cli/cli@2.60.0` with no lock completes: the
#      release by tag 200, synthesised, then the asset 200, and the
#      installed `gh --version` answers.
#  10. `mise install github:cli/cli@2.59.0` — a tag the instance does not
#      hold — is refused at the release by tag, filed as a *document* miss
#      naming v2.59.0 as requested and v2.60.0 as held, which `admin
#      air-gap-missing` prints as two columns (0008-bis §4.4, phase 2).
#  11. The miss log after the second half names none of the served listings
#      and nothing the bundle carried: that one unheld tag, and pip's own
#      `/simple/pip/` self-check.
#
# The elapsed time of each failing install is recorded too: a client that
# treats a 503 index as an outage retries, and how long it retries is part
# of what an operator sees (0008-bis §2 item 2).
#
# As in `mise.sh` §4, egress is denied to the processes under test (the
# server by `[air_gap]`, the clients by a proxy pointed at a closed port),
# not to the host: the connected side has to reach the three upstreams to
# have anything to carry across.
#
# Run via `task test:airgap-heavy` or directly. Needs network for the seed
# (registry.npmjs.org, pypi.org, and api.github.com anonymously — about
# three of the sixty requests an hour). Environment knobs: DATABASE_URL
# (required), HEAVY_PORT (8110, the connected instance), HEAVY_TAP_PORT
# (8119, in front of the disconnected instance), HEAVY_TF_TAP_PORT (8121, a
# TLS tap in front of the same instance, for Terraform's host-routed
# discovery), HEAVY_AIRGAP_PORT (8120,
# the disconnected instance), HEAVY_NPM_PKG (left-pad), HEAVY_NPM_VERSION
# (1.3.0), HEAVY_NPM_UNHELD_VERSION (1.2.0), HEAVY_PIP_PKG (six), HEAVY_PIP_VERSION (1.17.0), HEAVY_PIP_WHEEL
# (six-1.17.0-py2.py3-none-any.whl), HEAVY_MISE_TOOL (github:cli/cli),
# HEAVY_MISE_VERSION (2.60.0), HEAVY_MISE_UNHELD_VERSION (2.59.0).
set -euo pipefail

# shellcheck source=tests/heavy/lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

heavy_init airgap 8110 8119
heavy_need curl "curl"
heavy_need npm "nodejs"
heavy_need python3 "python3 with venv (the tap, and pip)"
heavy_runner_for mise "mise@latest"
MISE=("${HEAVY_RUNNER[@]}" mise)
# Phase 3's four clients, found the way their own suites find them.
heavy_need cargo "the Rust toolchain"
heavy_need go "the Go toolchain"
# mvn and dotnet are resolved to their binaries *now*, before this suite
# redirects mise's home and rewrite rules at the disconnected instance for
# its own rows: a `mise x maven@…` prefix evaluated later would try to
# install Maven through an instance that holds no Maven, with egress denied.
heavy_runner_for mvn maven@3.9.16 java@temurin-21.0.11+10.0.LTS
MVN_BIN="$("${HEAVY_RUNNER[@]}" bash -c 'command -v mvn')"
MVN_JAVA_HOME="$("${HEAVY_RUNNER[@]}" bash -c 'printf %s "${JAVA_HOME:-}"')"
MVN=(env JAVA_HOME="$MVN_JAVA_HOME" "$MVN_BIN")
DOTNET_VERSION="${DOTNET_VERSION:-10}"
heavy_runner_for dotnet "dotnet@$DOTNET_VERSION"
DOTNET_BIN="$("${HEAVY_RUNNER[@]}" bash -c 'command -v dotnet')"
DOTNET=("$DOTNET_BIN")
heavy_log "mvn at $MVN_BIN (JAVA_HOME=$MVN_JAVA_HOME), dotnet at $DOTNET_BIN"
# Ruby and Bundler, the way bundler.sh finds them; resolved to a binary for
# the same reason as mvn.
RUBY_VERSION="${RUBY_VERSION:-3.3}"
BUNDLER_VERSION="${BUNDLER_VERSION:-4.0.17}"
heavy_runner_for ruby "ruby@$RUBY_VERSION"
RUBY_BIN_DIR="$(dirname "$("${HEAVY_RUNNER[@]}" bash -c 'command -v ruby')")"
if ! "$RUBY_BIN_DIR/gem" list -i bundler -v "$BUNDLER_VERSION" >/dev/null 2>&1; then
  heavy_log "Installing bundler $BUNDLER_VERSION"
  "$RUBY_BIN_DIR/gem" install bundler -v "$BUNDLER_VERSION" --no-document >/dev/null
fi
BUNDLE=("$RUBY_BIN_DIR/bundle" "_${BUNDLER_VERSION}_")
# micromamba, as conda.sh gets it: one archive, cached across runs.
MICROMAMBA_VERSION="${MICROMAMBA_VERSION:-2.9.0}"
MM_DIR="$(heavy_cached_dir "micromamba-$MICROMAMBA_VERSION" \
  "https://micro.mamba.pm/api/micromamba/linux-64/$MICROMAMBA_VERSION" tar.bz2)"
MM="$MM_DIR/bin/micromamba"
[[ -x "$MM" ]] || heavy_fail "micromamba was not where the archive was expected to put it ($MM)"
# Terraform, as terraform.sh gets it.
TERRAFORM_VERSION="${TERRAFORM_VERSION:-1.8.5}"
heavy_runner_for terraform "terraform@$TERRAFORM_VERSION"
TF=("${HEAVY_RUNNER[@]}" terraform)
TF_TAP_PORT="${HEAVY_TF_TAP_PORT:-8121}"

GH_REG="github-$HEAVY_RUN"
NPM_REG="npm-$HEAVY_RUN"
PIP_REG="pypi-$HEAVY_RUN"
CARGO_REG="cargo-$HEAVY_RUN"
GO_REG="go-$HEAVY_RUN"
MVN_REG="mvn-$HEAVY_RUN"
NUGET_REG="nuget-$HEAVY_RUN"
GEMS_REG="gems-$HEAVY_RUN"
CONDA_REG="conda-$HEAVY_RUN"
TF_REG="tf-$HEAVY_RUN"
# One dependency-free version of each, so the client's resolution is the
# listing and the artifact, nothing else.
CRATE="${HEAVY_CARGO_CRATE:-unicode-xid}"
CRATE_VERSION="${HEAVY_CARGO_VERSION:-0.2.5}"
GO_MODULE="${HEAVY_GO_MODULE:-github.com/google/uuid}"
GO_VERSION="${HEAVY_GO_VERSION:-v1.5.0}"
MVN_GROUP="${HEAVY_MAVEN_GROUP:-org.apache.commons}"
MVN_ARTIFACT="${HEAVY_MAVEN_ARTIFACT:-commons-lang3}"
MVN_VERSION="${HEAVY_MAVEN_VERSION:-3.12.0}"
MVN_GROUP_PATH="${MVN_GROUP//.//}"
NUGET_PKG="${HEAVY_NUGET_PKG:-newtonsoft.json}"
NUGET_VERSION="${HEAVY_NUGET_VERSION:-13.0.3}"
GEM="${HEAVY_GEM:-rake}"
GEM_VERSION="${HEAVY_GEM_VERSION:-13.2.1}"
CONDA_PKG="${HEAVY_CONDA_PKG:-_libgcc_mutex}"
CONDA_FILE="${HEAVY_CONDA_FILE:-_libgcc_mutex-0.1-conda_forge.tar.bz2}"
CONDA_SUBDIR="${HEAVY_CONDA_SUBDIR:-linux-64}"
TF_PROVIDER="${HEAVY_TF_PROVIDER:-hashicorp/null}"
TF_PROVIDER_VERSION="${HEAVY_TF_PROVIDER_VERSION:-3.2.2}"
TF_NS="${TF_PROVIDER%%/*}"
TF_TYPE="${TF_PROVIDER##*/}"

NPM_PKG="${HEAVY_NPM_PKG:-left-pad}"
NPM_VERSION="${HEAVY_NPM_VERSION:-1.3.0}"
# A version upstream has and this instance does not: what a synthesised
# listing must *not* name.
NPM_UNHELD_VERSION="${HEAVY_NPM_UNHELD_VERSION:-1.2.0}"
PIP_PKG="${HEAVY_PIP_PKG:-six}"
PIP_VERSION="${HEAVY_PIP_VERSION:-1.17.0}"
PIP_WHEEL="${HEAVY_PIP_WHEEL:-six-1.17.0-py2.py3-none-any.whl}"
TOOL="${HEAVY_MISE_TOOL:-github:cli/cli}"
TOOL_VERSION="${HEAVY_MISE_VERSION:-2.60.0}"
# A tag upstream has and this instance does not.
TOOL_UNHELD_VERSION="${HEAVY_MISE_UNHELD_VERSION:-2.59.0}"
OWNER_REPO="${TOOL#github:}"
AIRGAP_PORT="${HEAVY_AIRGAP_PORT:-8120}"

# The measurement rows, printed at the end and copied into 0008-bis §13.1.
MEASURE_FILE="$HEAVY_WORK/measure.txt"
: > "$MEASURE_FILE"
measure() { printf '%s\n' "$*" | tee -a "$MEASURE_FILE"; }

# The client-side egress denial, as in mise.sh §4: a proxy on a closed port,
# with the loopback exempted so the tap is reachable.
DENY=(env HTTP_PROXY="http://127.0.0.1:1" HTTPS_PROXY="http://127.0.0.1:1"
      http_proxy="http://127.0.0.1:1" https_proxy="http://127.0.0.1:1"
      NO_PROXY="127.0.0.1,localhost" no_proxy="127.0.0.1,localhost")

# ── 0. The connected side: seed one version of each, export a bundle ────────

heavy_start_server tests/heavy/config.airgap.toml

cargo build --quiet -p batlehub-cli >"$HEAVY_WORK/cli-build.txt" 2>&1 \
  || { cat "$HEAVY_WORK/cli-build.txt" >&2; heavy_fail "the CLI did not build"; }
CLI="$(cargo metadata --format-version 1 --no-deps \
  | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/debug/batlehub-cli"
[[ -x "$CLI" ]] || heavy_fail "no batlehub-cli binary at $CLI"
export BATLEHUB_SERVER="$HEAVY_BASE"
export BATLEHUB_TOKEN="$ADMIN_TOKEN"

# The forge entry comes from a lock, the way RFC 0008 §4.2 means it to. The
# npm and PyPI entries are appended by hand: a plan is a list of proxy paths
# and `mise plan` only reads a mise.lock, so the two package registries have
# no planner yet — which is itself a row for 0008-bis (its §6.4 CLI item).
ASSET_URL="https://github.com/$OWNER_REPO/releases/download/v$TOOL_VERSION/gh_${TOOL_VERSION}_linux_amd64.tar.gz"
cat > "$HEAVY_WORK/mise.lock" <<EOF
[[tools."$TOOL"]]
version = "$TOOL_VERSION"
backend = "$TOOL"

[tools."$TOOL"."platforms.linux-x64"]
url = "$ASSET_URL"
EOF
"$CLI" mise plan --lock "$HEAVY_WORK/mise.lock" --platform linux-x64 \
  -o "$HEAVY_WORK/plan.json" >"$HEAVY_WORK/plan.txt" 2>"$HEAVY_WORK/plan.err" \
  || { cat "$HEAVY_WORK/plan.txt" "$HEAVY_WORK/plan.err" >&2; heavy_fail "mise plan failed"; }
python3 - "$HEAVY_WORK/plan.json" "$NPM_REG" "$NPM_PKG" "$NPM_VERSION" "$PIP_REG" "$PIP_PKG" "$PIP_VERSION" "$PIP_WHEEL" \
  "$CARGO_REG" "$CRATE" "$CRATE_VERSION" "$GO_REG" "$GO_MODULE" "$GO_VERSION" \
  "$MVN_REG" "$MVN_GROUP" "$MVN_ARTIFACT" "$MVN_VERSION" "$NUGET_REG" "$NUGET_PKG" "$NUGET_VERSION" \
  "$GEMS_REG" "$GEM" "$GEM_VERSION" "$CONDA_REG" "$CONDA_PKG" "$CONDA_FILE" "$CONDA_SUBDIR" \
  "$TF_REG" "$TF_NS" "$TF_TYPE" "$TF_PROVIDER_VERSION" <<'PY'
import json, sys
(path, npm_reg, npm_pkg, npm_v, pip_reg, pip_pkg, pip_v, wheel,
 cargo_reg, crate, crate_v, go_reg, module, go_v,
 mvn_reg, group, artifact, mvn_v, nuget_reg, nuget_pkg, nuget_v,
 gems_reg, gem, gem_v, conda_reg, conda_pkg, conda_file, conda_subdir,
 tf_reg, tf_ns, tf_type, tf_v) = sys.argv[1:]
plan = json.load(open(path))
assert any(e.get("proxy_path") for e in plan["entries"]), plan
group_path = group.replace(".", "/")
def entry(tool, version, reg, kind, key, proxy_path, url):
    return {"tool": tool, "version": version, "platform": "any", "url": url,
            "registry": {"name": reg, "type": kind}, "key": key, "proxy_path": proxy_path}
plan["entries"] += [
    entry(f"cargo:{crate}", crate_v, cargo_reg, "cargo", f"{cargo_reg}/{crate}/{crate_v}",
          f"/proxy/{cargo_reg}/{crate}/{crate_v}/download",
          f"https://static.crates.io/crates/{crate}/{crate}-{crate_v}.crate"),
    entry(f"go:{module}", go_v, go_reg, "goproxy", f"{go_reg}/{module}/{go_v}",
          f"/proxy/{go_reg}/{module}/@v/{go_v}.zip", f"https://proxy.golang.org/{module}/@v/{go_v}.zip"),
    entry(f"go:{module} (mod)", go_v, go_reg, "goproxy", f"{go_reg}/{module}/{go_v}",
          f"/proxy/{go_reg}/{module}/@v/{go_v}.mod", f"https://proxy.golang.org/{module}/@v/{go_v}.mod"),
    entry(f"maven:{group}:{artifact}", mvn_v, mvn_reg, "maven", f"{mvn_reg}/{group}:{artifact}/{mvn_v}",
          f"/proxy/{mvn_reg}/maven2/{group_path}/{artifact}/{mvn_v}/{artifact}-{mvn_v}.jar",
          f"https://repo1.maven.org/maven2/{group_path}/{artifact}/{mvn_v}/{artifact}-{mvn_v}.jar"),
    entry(f"maven:{group}:{artifact} (pom)", mvn_v, mvn_reg, "maven", f"{mvn_reg}/{group}:{artifact}/{mvn_v}",
          f"/proxy/{mvn_reg}/maven2/{group_path}/{artifact}/{mvn_v}/{artifact}-{mvn_v}.pom",
          f"https://repo1.maven.org/maven2/{group_path}/{artifact}/{mvn_v}/{artifact}-{mvn_v}.pom"),
    entry(f"nuget:{nuget_pkg}", nuget_v, nuget_reg, "nuget", f"{nuget_reg}/{nuget_pkg}/{nuget_v}",
          f"/proxy/{nuget_reg}/nuget/v3/flat/{nuget_pkg}/{nuget_v}/{nuget_pkg}.{nuget_v}.nupkg",
          f"https://api.nuget.org/v3-flatcontainer/{nuget_pkg}/{nuget_v}/{nuget_pkg}.{nuget_v}.nupkg"),
    entry(f"gem:{gem}", gem_v, gems_reg, "rubygems", f"{gems_reg}/{gem}/{gem_v}",
          f"/proxy/{gems_reg}/gems/{gem}-{gem_v}.gem", f"https://rubygems.org/gems/{gem}-{gem_v}.gem"),
    entry(f"conda:{conda_pkg}", "0.1", conda_reg, "conda", f"{conda_reg}/{conda_pkg}/0.1",
          f"/proxy/{conda_reg}/{conda_subdir}/{conda_file}",
          f"https://conda.anaconda.org/conda-forge/{conda_subdir}/{conda_file}"),
]
# A provider is three artifacts (0008-bis §13.7): the archive, and the
# checksum list and its signature that Terraform verifies it against.
tf_prefix = f"/proxy/{tf_reg}/v1/providers/{tf_ns}/{tf_type}/{tf_v}"
tf_key = f"{tf_reg}/providers/{tf_ns}/{tf_type}/{tf_v}"
tf_release = f"https://releases.hashicorp.com/terraform-provider-{tf_type}/{tf_v}/terraform-provider-{tf_type}_{tf_v}"
plan["entries"] += [
    entry(f"terraform:{tf_ns}/{tf_type}", tf_v, tf_reg, "terraform", tf_key,
          f"{tf_prefix}/artifact/linux/amd64", f"{tf_release}_linux_amd64.zip"),
    entry(f"terraform:{tf_ns}/{tf_type} (shasums)", tf_v, tf_reg, "terraform", tf_key,
          f"{tf_prefix}/shasums", f"{tf_release}_SHA256SUMS"),
    entry(f"terraform:{tf_ns}/{tf_type} (shasums.sig)", tf_v, tf_reg, "terraform", tf_key,
          f"{tf_prefix}/shasums.sig", f"{tf_release}_SHA256SUMS.sig"),
]
plan["entries"] += [
    {
        "tool": f"npm:{npm_pkg}", "version": npm_v, "platform": "any",
        "url": f"https://registry.npmjs.org/{npm_pkg}/-/{npm_pkg}-{npm_v}.tgz",
        "registry": {"name": npm_reg, "type": "npm"},
        "key": f"{npm_reg}/{npm_pkg}/{npm_v}",
        "proxy_path": f"/proxy/{npm_reg}/{npm_pkg}/{npm_v}/tarball",
    },
    {
        "tool": f"pypi:{pip_pkg}", "version": pip_v, "platform": "any",
        "url": f"https://files.pythonhosted.org/packages/{wheel}",
        "registry": {"name": pip_reg, "type": "pypi"},
        "key": f"{pip_reg}/{pip_pkg}/{pip_v}",
        "proxy_path": f"/proxy/{pip_reg}/packages/{wheel}",
    },
]
json.dump(plan, open(path, "w"), indent=2)
print(f"plan: {len(plan['entries'])} entries")
PY
heavy_log "AIRGAP-PLAN-OK ($(grep -c '"proxy_path"' "$HEAVY_WORK/plan.json") planned paths)"

"$CLI" mise seed --plan "$HEAVY_WORK/plan.json" >"$HEAVY_WORK/seed.txt" 2>"$HEAVY_WORK/seed.err" \
  || { cat "$HEAVY_WORK/seed.txt" "$HEAVY_WORK/seed.err" >&2; heavy_fail "mise seed failed — a planned path the server does not answer"; }
heavy_log "AIRGAP-SEED-OK ($(head -1 "$HEAVY_WORK/seed.txt"))"

head -c 32 /dev/urandom | od -An -tx1 | tr -d " \n" > "$HEAVY_WORK/estate.key"
"$CLI" --json mise export --plan "$HEAVY_WORK/plan.json" \
  --sign-key "$HEAVY_WORK/estate.key" -o "$HEAVY_WORK/estate.bhub" \
  --bundle-id "heavy-airgap-$HEAVY_RUN" >"$HEAVY_WORK/export.json" 2>"$HEAVY_WORK/export.err" \
  || { cat "$HEAVY_WORK/export.json" "$HEAVY_WORK/export.err" >&2; heavy_fail "mise export failed"; }
BLOBS="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["blobs"])' "$HEAVY_WORK/export.json")"
[[ "$BLOBS" -eq 14 ]] || { cat "$HEAVY_WORK/export.json" "$HEAVY_WORK/export.err" >&2; heavy_fail "the bundle carried $BLOBS blob(s), expected 14 (asset, tarball, wheel, crate, zip, mod, jar, pom, nupkg, gem, conda package, provider archive, its checksum list and signature)"; }
# The export read the provider's download document and carried its facts.
python3 - "$HEAVY_WORK/estate.bhub" "$TF_REG" <<'PY' || heavy_fail "the manifest does not carry the provider's signing keys (0008-bis §13.7)"
import json, sys, tarfile
bundle, tf_reg = sys.argv[1:]
with tarfile.open(bundle) as t:
    entries = json.load(t.extractfile("manifest.json"))["entries"]
archives = [e for e in entries if e["registry"] == tf_reg and e["key"].endswith("/linux/amd64")]
assert len(archives) == 1, [e["key"] for e in entries if e["registry"] == tf_reg]
facts = archives[0].get("facts", {}).get("terraform", {})
keys = facts.get("signing_keys", {}).get("gpg_public_keys", [])
assert keys and all(k.get("key_id") and k.get("ascii_armor") for k in keys), facts
print(f"provider facts: protocols={facts.get('protocols')} keys={[k['key_id'] for k in keys]}")
PY
export HEAVY_BUNDLE_PUBKEY="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["signer_key"])' "$HEAVY_WORK/export.json")"
heavy_log "AIRGAP-EXPORT-OK (14 blobs, signed by ${HEAVY_BUNDLE_PUBKEY:0:8}…)"

# Maven needs its plugins to resolve anything, and a disconnected instance
# holds none of them: the local repository is warmed against the *connected*
# instance first, the way `maven.sh` does, and copied for the disconnected
# run with the artifact under test removed. What the disconnected run then
# needs is exactly the listing and the two files the bundle carried.
heavy_log "Warming a Maven local repository against the connected instance (plugins and $MVN_ARTIFACT $MVN_VERSION)"
mkdir -p "$HEAVY_WORK/mvn-warm-project"
cat >"$HEAVY_WORK/settings-connected.xml" <<EOF
<settings>
  <mirrors>
    <mirror>
      <!-- The same id as the disconnected settings: Maven records which
           repository each cached file came from (_remote.repositories) and
           re-verifies a file against a repository it does not know. -->
      <id>airgap</id>
      <mirrorOf>*</mirrorOf>
      <url>$HEAVY_BASE/proxy/$MVN_REG/maven2</url>
    </mirror>
  </mirrors>
</settings>
EOF
cat >"$HEAVY_WORK/mvn-warm-project/pom.xml" <<EOF
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>example</groupId>
  <artifactId>warm</artifactId>
  <version>0.0.0</version>
  <dependencies>
    <dependency>
      <groupId>$MVN_GROUP</groupId>
      <artifactId>$MVN_ARTIFACT</artifactId>
      <version>$MVN_VERSION</version>
    </dependency>
  </dependencies>
</project>
EOF
(cd "$HEAVY_WORK/mvn-warm-project" && "${MVN[@]}" -B -q -s "$HEAVY_WORK/settings-connected.xml" \
  -Dmaven.repo.local="$HEAVY_WORK/mvn-warm" dependency:resolve) >"$HEAVY_WORK/mvn-warm.txt" 2>&1 \
  || { tail -30 "$HEAVY_WORK/mvn-warm.txt" >&2; heavy_fail "mvn dependency:resolve through the connected instance failed"; }
heavy_log "MVN-WARM-OK"

# ── 1. The disconnected side, behind the tap ────────────────────────────────

# On a copy: the second half flips `synthesise_listings` and reloads, and
# the checked-in file must not change under a run.
AG_CONFIG="$HEAVY_WORK/config.airgap-disconnected.toml"
cp tests/heavy/config.airgap-disconnected.toml "$AG_CONFIG"
heavy_start_second_server "$AG_CONFIG" "$AIRGAP_PORT"
AG_BASE="$HEAVY_BASE2"

# The tap in front of the *disconnected* instance: every client below is
# pointed at it, so the transcript is what they asked a server with no
# route out.
python3 tests/heavy/http_tap.py "$HEAVY_LOG" "$HEAVY_TAP_PORT" "$AIRGAP_PORT" >"$HEAVY_WORK/tap.err" 2>&1 &
HEAVY_TAP_PID=$!
for _ in $(seq 1 30); do
  curl -s -o /dev/null "$HEAVY_TAP_BASE/healthz" && break
  sleep 1
done
curl -s -o /dev/null "$HEAVY_TAP_BASE/healthz" \
  || { cat "$HEAVY_WORK/tap.err" >&2; heavy_fail "the tap never came up on $HEAVY_TAP_PORT"; }
heavy_log "Tap listening on $HEAVY_TAP_PORT -> $AIRGAP_PORT (the disconnected instance)"

# A second tap, TLS, in front of the same instance: Terraform reaches a
# registry only on a host of its own and only over https (terraform.sh),
# and `localhost` is bound to the Terraform registry in the disconnected
# config. Self-signed, trusted for this run through SSL_CERT_FILE.
export HEAVY_TAP_HOST=localhost
TF_CERT="$(heavy_self_signed localhost)"
python3 tests/heavy/http_tap.py "$HEAVY_LOG" "$TF_TAP_PORT" "$AIRGAP_PORT" "$TF_CERT" "$HEAVY_WORK/tls-key.pem" \
  >"$HEAVY_WORK/tf-tap.err" 2>&1 &
HEAVY_EXTRA_PIDS+=($!)
TF_HOST="localhost:$TF_TAP_PORT"
for _ in $(seq 1 30); do
  curl -s -o /dev/null --cacert "$TF_CERT" "https://$TF_HOST/healthz" && break
  sleep 1
done
curl -s -o /dev/null --cacert "$TF_CERT" "https://$TF_HOST/healthz" \
  || { cat "$HEAVY_WORK/tf-tap.err" >&2; heavy_fail "the TLS tap never came up on $TF_TAP_PORT"; }
heavy_log "TLS tap listening on $TF_TAP_PORT -> $AIRGAP_PORT (Terraform's host)"

BATLEHUB_SERVER="$AG_BASE" "$CLI" mise import "$HEAVY_WORK/estate.bhub" \
  >"$HEAVY_WORK/import.txt" 2>"$HEAVY_WORK/import.err" \
  || { cat "$HEAVY_WORK/import.txt" "$HEAVY_WORK/import.err" >&2; heavy_fail "mise import failed"; }
grep -q "signature ok" "$HEAVY_WORK/import.txt" \
  || { cat "$HEAVY_WORK/import.txt" >&2; heavy_fail "the import did not verify the signature"; }
heavy_log "AIRGAP-IMPORT-OK ($(head -1 "$HEAVY_WORK/import.txt"))"

# Held, by the read path: the three artifacts answer 200 from an instance
# that cannot dial. This is the premise of every row below.
for p in "/proxy/$NPM_REG/$NPM_PKG/$NPM_VERSION/tarball" \
         "/proxy/$PIP_REG/packages/$PIP_WHEEL" \
         "/proxy/$GH_REG/$OWNER_REPO/releases/download/v$TOOL_VERSION/gh_${TOOL_VERSION}_linux_amd64.tar.gz" \
         "/proxy/$CARGO_REG/$CRATE/$CRATE_VERSION/download" \
         "/proxy/$GO_REG/$GO_MODULE/@v/$GO_VERSION.zip" \
         "/proxy/$GO_REG/$GO_MODULE/@v/$GO_VERSION.mod" \
         "/proxy/$MVN_REG/maven2/$MVN_GROUP_PATH/$MVN_ARTIFACT/$MVN_VERSION/$MVN_ARTIFACT-$MVN_VERSION.jar" \
         "/proxy/$MVN_REG/maven2/$MVN_GROUP_PATH/$MVN_ARTIFACT/$MVN_VERSION/$MVN_ARTIFACT-$MVN_VERSION.pom" \
         "/proxy/$NUGET_REG/nuget/v3/flat/$NUGET_PKG/$NUGET_VERSION/$NUGET_PKG.$NUGET_VERSION.nupkg" \
         "/proxy/$GEMS_REG/gems/$GEM-$GEM_VERSION.gem" \
         "/proxy/$CONDA_REG/$CONDA_SUBDIR/$CONDA_FILE"; do
  code="$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $ADMIN_TOKEN" "$AG_BASE$p")"
  [[ "$code" == "200" ]] || heavy_fail "the disconnected instance answered $code on $p, which the bundle carried"
done
heavy_log "AIRGAP-HELD-OK (all eleven artifacts 200 on the disconnected instance)"

NPM_URL="$HEAVY_TAP_BASE/proxy/$NPM_REG/"
PIP_SIMPLE="$HEAVY_TAP_BASE/proxy/$PIP_REG/simple/"
PIP_WHEEL_URL="$HEAVY_TAP_BASE/proxy/$PIP_REG/packages/$PIP_WHEEL"
GH_PROXY="$HEAVY_TAP_BASE/proxy/$GH_REG"

# npm: anonymous reads are allowed by the config, so no token; its cache
# inside the run, one per row, so nothing answers from memory.
export NPM_CONFIG_USERCONFIG="$HEAVY_WORK/npmrc"
cat > "$NPM_CONFIG_USERCONFIG" <<EOF
registry=$NPM_URL
update-notifier=false
fund=false
audit=false
EOF

# run_client <label> <log> <cmd...> — run under egress denial, keep the
# output and the exit code, and time it.
run_client() {
  local label="$1" log="$2"
  shift 2
  local t0 t1
  t0=$(date +%s)
  set +e
  "${DENY[@]}" "$@" >"$log" 2>&1
  CLIENT_RC=$?
  set -e
  t1=$(date +%s)
  CLIENT_SECS=$((t1 - t0))
  heavy_log "$label: exit $CLIENT_RC after ${CLIENT_SECS}s"
}

# ── 2. npm: a version string resolves through the packument ─────────────────

heavy_mark "npm-fresh"
FRESH="$HEAVY_WORK/npm-fresh"
mkdir -p "$FRESH"
heavy_log "npm install $NPM_PKG@$NPM_VERSION from a clean cache, against the disconnected instance"
run_client "npm install" "$HEAVY_WORK/npm-fresh.txt" \
  env npm_config_cache="$FRESH/cache" npm --prefix "$FRESH" install "$NPM_PKG@$NPM_VERSION"
[[ $CLIENT_RC -ne 0 ]] || heavy_fail "npm install $NPM_PKG@$NPM_VERSION succeeded against an instance that holds no packument — a listing was answered from somewhere"
heavy_wire_re_after "npm-fresh" "GET /proxy/$NPM_REG/$NPM_PKG -> 503" \
  "npm did not ask for the packument, or it was not the 503 an air-gapped miss promises"
if heavy_wire_seen_after "npm-fresh" "GET /proxy/$NPM_REG/$NPM_PKG/$NPM_VERSION/tarball"; then
  heavy_fail "npm asked for the tarball after a 503 on the packument — the version string did not need the listing after all"
fi
NPM_PACKUMENT_TRIES="$(heavy_wire_count_after npm-fresh "GET /proxy/$NPM_REG/$NPM_PKG -> 503")"
NPM_SAID="$(grep -E 'npm (error|ERR!) (code|[0-9]{3})' "$HEAVY_WORK/npm-fresh.txt" | head -2 | sed 's/^npm \(error\|ERR!\) //' | tr '\n' ';')"
measure "npm  | install $NPM_PKG@$NPM_VERSION, clean cache | GET /$NPM_PKG -> 503 x$NPM_PACKUMENT_TRIES, tarball never asked | exit $CLIENT_RC after ${CLIENT_SECS}s | $NPM_SAID"
heavy_log "NPM-FRESH-MEASURED"

# ── 3. npm: a lock resolves nothing ─────────────────────────────────────────

heavy_mark "npm-lock"
LOCKED="$HEAVY_WORK/npm-lock"
mkdir -p "$LOCKED"
cat > "$LOCKED/package.json" <<EOF
{ "name": "consumer", "version": "1.0.0", "private": true, "dependencies": { "$NPM_PKG": "$NPM_VERSION" } }
EOF
cat > "$LOCKED/package-lock.json" <<EOF
{
  "name": "consumer", "version": "1.0.0", "lockfileVersion": 3, "requires": true,
  "packages": {
    "": { "name": "consumer", "version": "1.0.0", "dependencies": { "$NPM_PKG": "$NPM_VERSION" } },
    "node_modules/$NPM_PKG": { "version": "$NPM_VERSION", "resolved": "$NPM_URL$NPM_PKG/$NPM_VERSION/tarball", "license": "MIT" }
  }
}
EOF
heavy_log "npm ci from a lockfile naming the held tarball"
run_client "npm ci" "$HEAVY_WORK/npm-lock.txt" \
  env npm_config_cache="$LOCKED/cache" npm --prefix "$LOCKED" ci
[[ $CLIENT_RC -eq 0 ]] || { cat "$HEAVY_WORK/npm-lock.txt" >&2; heavy_fail "npm ci from a lock naming the held tarball failed against the disconnected instance"; }
heavy_wire_re_after "npm-lock" "GET /proxy/$NPM_REG/$NPM_PKG/$NPM_VERSION/tarball -> 200" \
  "the locked install did not fetch the tarball from the disconnected instance"
if heavy_wire_seen_after "npm-lock" "GET /proxy/$NPM_REG/$NPM_PKG "; then
  heavy_fail "npm ci asked for the packument — the lock did not resolve nothing"
fi
[[ -f "$LOCKED/node_modules/$NPM_PKG/package.json" ]] || heavy_fail "npm ci left no $NPM_PKG in node_modules"
measure "npm  | ci from a lock naming the tarball | GET /$NPM_PKG/$NPM_VERSION/tarball -> 200, nothing else | exit 0 after ${CLIENT_SECS}s | installed"
heavy_log "NPM-LOCK-OK (the tarball, and nothing else)"

# ── 4. pip: a version string resolves through the simple page ───────────────

heavy_log "Creating the pip virtualenv ($(python3 --version))"
python3 -m venv "$HEAVY_WORK/venv" || heavy_fail "python3 -m venv failed (python3-venv missing?)"
PY="$HEAVY_WORK/venv/bin/python"
PIP_VERSION_STR="$("$PY" -m pip --version | cut -d' ' -f2)"

heavy_mark "pip-fresh"
heavy_log "pip install $PIP_PKG==$PIP_VERSION against the disconnected instance's simple index"
run_client "pip install" "$HEAVY_WORK/pip-fresh.txt" \
  "$PY" -m pip install --no-cache-dir --index-url "$PIP_SIMPLE" "$PIP_PKG==$PIP_VERSION"
[[ $CLIENT_RC -ne 0 ]] || heavy_fail "pip install $PIP_PKG==$PIP_VERSION succeeded against an instance that holds no simple page"
heavy_wire_re_after "pip-fresh" "GET /proxy/$PIP_REG/simple/$PIP_PKG/ -> 503" \
  "pip did not ask for the simple page, or it was not a 503"
if heavy_wire_seen_after "pip-fresh" "GET /proxy/$PIP_REG/packages/$PIP_WHEEL"; then
  heavy_fail "pip asked for the wheel after a 503 on the simple page"
fi
PIP_SIMPLE_TRIES="$(heavy_wire_count_after pip-fresh "GET /proxy/$PIP_REG/simple/$PIP_PKG/ -> 503")"
PIP_SAID="$(grep -E '^(ERROR|WARNING): ' "$HEAVY_WORK/pip-fresh.txt" | sed 's/ *$//' | sort | uniq -c | sort -rn | head -3 | sed 's/^ *//' | tr '\n' ';')"
measure "pip  | install $PIP_PKG==$PIP_VERSION, no cache | GET /simple/$PIP_PKG/ -> 503 x$PIP_SIMPLE_TRIES, wheel never asked | exit $CLIENT_RC after ${CLIENT_SECS}s | $PIP_SAID"
heavy_log "PIP-FRESH-MEASURED"

# ── 5. pip: a direct reference resolves nothing ─────────────────────────────

heavy_mark "pip-direct"
heavy_log "pip install '$PIP_PKG @ <held wheel URL>' — PEP 508's lock"
run_client "pip install (direct)" "$HEAVY_WORK/pip-direct.txt" \
  "$PY" -m pip install --no-cache-dir --index-url "$PIP_SIMPLE" "$PIP_PKG @ $PIP_WHEEL_URL"
[[ $CLIENT_RC -eq 0 ]] || { cat "$HEAVY_WORK/pip-direct.txt" >&2; heavy_fail "pip install of a direct reference to the held wheel failed against the disconnected instance"; }
heavy_wire_re_after "pip-direct" "GET /proxy/$PIP_REG/packages/$PIP_WHEEL -> 200" \
  "the direct install did not fetch the wheel from the disconnected instance"
PIP_DIRECT_INDEX="$(heavy_wire_count_after pip-direct "GET /proxy/$PIP_REG/simple/$PIP_PKG/")"
# pip's own version self-check (`/simple/pip/`) is a request on the index
# that has nothing to do with the install and is ignored when it fails; it
# is counted apart so the row above is about the package.
PIP_SELFCHECK="$(heavy_wire_count_after pip-direct "GET /proxy/$PIP_REG/simple/pip/")"
"$PY" -c "import $PIP_PKG; print($PIP_PKG.__version__)" | grep -qx "$PIP_VERSION" \
  || heavy_fail "$PIP_PKG did not import at $PIP_VERSION after the direct install"
measure "pip  | install '$PIP_PKG @ <wheel URL>' | GET /packages/$PIP_WHEEL -> 200, /simple/$PIP_PKG/ asked $PIP_DIRECT_INDEX time(s), /simple/pip/ (the self-check) $PIP_SELFCHECK time(s) | exit 0 after ${CLIENT_SECS}s | installed"
heavy_log "PIP-DIRECT-OK (the wheel; /simple/$PIP_PKG/ asked $PIP_DIRECT_INDEX time(s))"

# ── 6. mise: a version string resolves through the release listing ──────────

export MISE_DATA_DIR="$HEAVY_WORK/mise/data"
export MISE_CACHE_DIR="$HEAVY_WORK/mise/cache"
export MISE_CONFIG_DIR="$HEAVY_WORK/mise/config"
export MISE_STATE_DIR="$HEAVY_WORK/mise/state"
export MISE_YES=1
export MISE_TRUSTED_CONFIG_PATHS="$HEAVY_WORK"
unset GITHUB_TOKEN GH_TOKEN MISE_GITHUB_TOKEN
mkdir -p "$MISE_DATA_DIR" "$MISE_CACHE_DIR" "$MISE_CONFIG_DIR" "$MISE_STATE_DIR"
# Four backslashes, for the reason mise.sh §4 gives: the heredoc eats one
# pair and TOML needs `\\.`; two reach mise as a TOML error it reports and
# then ignores, dropping the whole settings block.
cat > "$MISE_CONFIG_DIR/config.toml" <<EOF
[settings]
github_attestations = false

[settings.github]
slsa = false

[settings.aqua]
cosign = false
slsa = false
minisign = false

[settings.url_replacements]
"regex:^https://api\\\\.github\\\\.com/repos/(.+)" = "$GH_PROXY/\$1"
"regex:^https://github\\\\.com/([^/]+)/([^/]+)/releases/download/([^/]+)/(.+)" = "$GH_PROXY/\$1/\$2/releases/download/\$3/\$4"
"regex:^https://codeload\\\\.github\\\\.com/([^/]+)/([^/]+)/tar\\\\.gz/(?:refs/tags/)?(.+)" = "$GH_PROXY/\$1/\$2/tarball/\$3"
EOF

heavy_mark "mise-nolock"
NOLOCK="$HEAVY_WORK/mise-nolock"
mkdir -p "$NOLOCK"
cat > "$NOLOCK/mise.toml" <<EOF
[tools]
"$TOOL" = { version = "$TOOL_VERSION", exe = "gh" }
EOF
heavy_log "mise install $TOOL@$TOOL_VERSION with no lock, against the disconnected instance"
run_client "mise install" "$HEAVY_WORK/mise-nolock.txt" \
  bash -c "cd '$NOLOCK' && ${MISE[*]} install"
[[ $CLIENT_RC -ne 0 ]] || heavy_fail "mise install with no lock succeeded against an instance that holds no release listing"
heavy_wire_re_after "mise-nolock" "GET /proxy/$GH_REG/$OWNER_REPO/releases/tags/v$TOOL_VERSION -> 503" \
  "mise did not ask for the release by tag, or it was not a 503"
heavy_wire_re_after "mise-nolock" "GET /proxy/$GH_REG/$OWNER_REPO/releases\\?per_page=100 -> 503" \
  "mise did not fall back to the release listing, or it was not a 503"
if heavy_wire_seen_after "mise-nolock" "/releases/download/v$TOOL_VERSION/"; then
  heavy_fail "mise asked for the asset after a 503 on the listing — the version string did not need the listing after all"
fi
MISE_FIRST="$(awk -v mark="### mise-nolock" 'index($0, mark) == 1 { seen = 1; next } seen && /GET \/proxy/ { print; exit }' "$HEAVY_LOG" | sed -E 's/ -> .*//')"
MISE_BYTAG="$(heavy_wire_count_after mise-nolock "GET /proxy/$GH_REG/$OWNER_REPO/releases/tags/v$TOOL_VERSION -> 503")"
MISE_LIST="$(heavy_wire_count_after mise-nolock "GET /proxy/$GH_REG/$OWNER_REPO/releases\\?per_page=100 -> 503")"
MISE_SAID="$(grep -E 'mise ERROR' "$HEAVY_WORK/mise-nolock.txt" | tail -3 | sed -E 's/^mise ERROR +//' | cut -c1-160 | tr '\n' ';')"
measure "mise | install $TOOL@$TOOL_VERSION, no lock | first $MISE_FIRST -> 503; by-tag x$MISE_BYTAG, listing x$MISE_LIST, asset never asked | exit $CLIENT_RC after ${CLIENT_SECS}s | $MISE_SAID"
heavy_log "MISE-NOLOCK-MEASURED"

# ── 7. Synthesis on: the same commands, answered ────────────────────────────
#
# RFC 0008-bis phase 1. The miss log is purged first so what it says at the
# end is about this half alone, then the copy of the config is rewritten —
# beside and renamed over, one change event for the watcher — and reloaded.

# `before` is required for a purge that means "everything": without it the
# endpoint forgets only rows older than the retention, which is nothing.
for r in "$NPM_REG" "$PIP_REG" "$GH_REG" "$CARGO_REG" "$GO_REG" "$MVN_REG" "$NUGET_REG" "$GEMS_REG" "$CONDA_REG" "$TF_REG"; do
  curl -fsS -o /dev/null -X DELETE -H "Authorization: Bearer $ADMIN_TOKEN" \
    "$AG_BASE/api/v1/admin/air-gap/missing?registry=$r&before=2099-01-01T00:00:00Z" \
    || heavy_fail "could not purge the miss log for $r"
done
heavy_log "Flipping synthesise_listings to true and reloading the disconnected instance"
python3 - "$AG_CONFIG" <<'PY' || heavy_fail "could not rewrite the config copy"
import os, sys
path = sys.argv[1]
text = open(path).read()
assert "synthesise_listings = false" in text, "nothing to flip"
open(path + ".next", "w").write(text.replace("synthesise_listings = false", "synthesise_listings = true", 1))
os.replace(path + ".next", path)
PY
sleep 2
RELOAD_CODE="$(curl -sS -o "$HEAVY_WORK/reload.json" -w '%{http_code}' -X POST \
  "$AG_BASE/api/v1/admin/config/reload" -H "Authorization: Bearer $ADMIN_TOKEN")"
[[ "$RELOAD_CODE" == "200" ]] \
  || { cat "$HEAVY_WORK/reload.json" >&2; heavy_fail "config reload answered $RELOAD_CODE"; }
# The reload took when the packument answers: polled, because the watcher
# and the explicit reload race and the second one to run answers 400.
code=""
for _ in $(seq 1 15); do
  code="$(curl -s -o /dev/null -w '%{http_code}' "$AG_BASE/proxy/$NPM_REG/$NPM_PKG")"
  [[ "$code" == "200" ]] && break
  sleep 1
done
[[ "$code" == "200" ]] \
  || heavy_fail "after the reload the packument still answers $code — synthesise_listings did not take"
heavy_log "AIRGAP-RELOAD-OK (the disconnected instance now synthesises listings)"

heavy_mark "npm-synth"
SYNTH="$HEAVY_WORK/npm-synth"
mkdir -p "$SYNTH"
heavy_log "npm install $NPM_PKG@$NPM_VERSION from a clean cache, against the synthesised packument"
run_client "npm install (synthesised)" "$HEAVY_WORK/npm-synth.txt" \
  env npm_config_cache="$SYNTH/cache" npm --prefix "$SYNTH" install "$NPM_PKG@$NPM_VERSION"
[[ $CLIENT_RC -eq 0 ]] || { cat "$HEAVY_WORK/npm-synth.txt" >&2; heavy_fail "npm install $NPM_PKG@$NPM_VERSION failed against the synthesised packument"; }
heavy_wire_re_after "npm-synth" "GET /proxy/$NPM_REG/$NPM_PKG -> 200 .*X-BatleHub-Listing: synthesised" \
  "the packument was not answered as synthesised"
heavy_wire_re_after "npm-synth" "GET /proxy/$NPM_REG/$NPM_PKG/$NPM_VERSION/tarball -> 200" \
  "npm did not fetch the tarball the synthesised packument named"
[[ -f "$SYNTH/node_modules/$NPM_PKG/package.json" ]] || heavy_fail "npm install left no $NPM_PKG in node_modules"
measure "npm  | install $NPM_PKG@$NPM_VERSION, clean cache, synthesis on | GET /$NPM_PKG -> 200 synthesised, then the tarball -> 200 | exit 0 after ${CLIENT_SECS}s | installed"
heavy_log "NPM-SYNTH-OK (the version string resolved through a listing this instance composed)"

# A version the instance does not hold: the synthesised packument does not
# name it, so npm stops by itself — ETARGET — and asks for no tarball. The
# listing named only what is held, which is the invariant of 0008-bis §5.1.
heavy_mark "npm-unheld"
UNHELD="$HEAVY_WORK/npm-unheld"
mkdir -p "$UNHELD"
heavy_log "npm install $NPM_PKG@$NPM_UNHELD_VERSION — a version the instance does not hold"
run_client "npm install (unheld)" "$HEAVY_WORK/npm-unheld.txt" \
  env npm_config_cache="$UNHELD/cache" npm --prefix "$UNHELD" install "$NPM_PKG@$NPM_UNHELD_VERSION"
[[ $CLIENT_RC -ne 0 ]] || heavy_fail "npm install $NPM_PKG@$NPM_UNHELD_VERSION succeeded — the synthesised packument named a version the instance does not hold"
grep -q "ETARGET" "$HEAVY_WORK/npm-unheld.txt" \
  || { cat "$HEAVY_WORK/npm-unheld.txt" >&2; heavy_fail "npm did not fail with ETARGET on the unheld version"; }
if heavy_wire_seen_after "npm-unheld" "GET /proxy/$NPM_REG/$NPM_PKG/$NPM_UNHELD_VERSION/tarball"; then
  heavy_fail "npm asked for the unheld tarball — the synthesised packument named it"
fi
measure "npm  | install $NPM_PKG@$NPM_UNHELD_VERSION (unheld), synthesis on | GET /$NPM_PKG -> 200 synthesised, no tarball asked | exit $CLIENT_RC after ${CLIENT_SECS}s | ETARGET"
heavy_log "NPM-UNHELD-OK (ETARGET off the synthesised packument, no request for the missing tarball)"

heavy_mark "pip-synth"
python3 -m venv "$HEAVY_WORK/venv-synth" || heavy_fail "python3 -m venv failed"
PY2="$HEAVY_WORK/venv-synth/bin/python"
heavy_log "pip install $PIP_PKG==$PIP_VERSION against the synthesised simple page"
run_client "pip install (synthesised)" "$HEAVY_WORK/pip-synth.txt" \
  "$PY2" -m pip install --no-cache-dir --index-url "$PIP_SIMPLE" "$PIP_PKG==$PIP_VERSION"
[[ $CLIENT_RC -eq 0 ]] || { cat "$HEAVY_WORK/pip-synth.txt" >&2; heavy_fail "pip install $PIP_PKG==$PIP_VERSION failed against the synthesised simple page"; }
heavy_wire_re_after "pip-synth" "GET /proxy/$PIP_REG/simple/$PIP_PKG/ -> 200 .*X-BatleHub-Listing: synthesised" \
  "the simple page was not answered as synthesised"
heavy_wire_re_after "pip-synth" "GET /proxy/$PIP_REG/packages/$PIP_WHEEL -> 200" \
  "pip did not fetch the wheel the synthesised page named"
"$PY2" -c "import $PIP_PKG; print($PIP_PKG.__version__)" | grep -qx "$PIP_VERSION" \
  || heavy_fail "$PIP_PKG did not import at $PIP_VERSION after the synthesised install"
measure "pip  | install $PIP_PKG==$PIP_VERSION, no cache, synthesis on | GET /simple/$PIP_PKG/ -> 200 synthesised, then the wheel -> 200 | exit 0 after ${CLIENT_SECS}s | installed"
heavy_log "PIP-SYNTH-OK (the version string resolved through a listing this instance composed)"

heavy_mark "mise-synth"
SYNTHTOOL="$HEAVY_WORK/mise-synth"
mkdir -p "$SYNTHTOOL"
cp "$NOLOCK/mise.toml" "$SYNTHTOOL/mise.toml"
heavy_log "mise install $TOOL@$TOOL_VERSION with no lock, against the synthesised release"
run_client "mise install (synthesised)" "$HEAVY_WORK/mise-synth.txt" \
  bash -c "cd '$SYNTHTOOL' && ${MISE[*]} install"
[[ $CLIENT_RC -eq 0 ]] || { cat "$HEAVY_WORK/mise-synth.txt" >&2; heavy_fail "mise install with no lock failed against the synthesised release — read the log above: an asset address the composed release does not answer is the finding"; }
heavy_wire_re_after "mise-synth" "GET /proxy/$GH_REG/$OWNER_REPO/releases/tags/v$TOOL_VERSION -> 200 .*X-BatleHub-Listing: synthesised" \
  "the release by tag was not answered as synthesised"
heavy_wire_re_after "mise-synth" "GET /proxy/$GH_REG/$OWNER_REPO/releases/download/v$TOOL_VERSION/[^ ]* -> 200" \
  "mise did not fetch the asset the composed release named"
MISE_INSTALLED="$(cd "$SYNTHTOOL" && "${DENY[@]}" "${MISE[@]}" exec -- gh --version 2>/dev/null | head -1 || true)"
[[ "$MISE_INSTALLED" == *"$TOOL_VERSION"* ]] \
  || heavy_fail "gh --version answered '$MISE_INSTALLED' after the synthesised install, expected $TOOL_VERSION"
MISE_ASKED="$(awk -v mark="### mise-synth" 'index($0, mark) == 1 { seen = 1; next } seen && /GET \/proxy/ { sub(/ -> .*/, ""); print }' "$HEAVY_LOG" | sed "s|GET /proxy/$GH_REG/$OWNER_REPO||" | tr '\n' ' ')"
measure "mise | install $TOOL@$TOOL_VERSION, no lock, synthesis on | asked:$MISE_ASKED| exit 0 after ${CLIENT_SECS}s | $MISE_INSTALLED"
heavy_log "MISE-SYNTH-OK ($MISE_INSTALLED, with no lock and no route off the host)"

# A tag the instance does not hold (RFC 0008-bis §4.4, phase 2): the release
# by tag is a 503 filed as a *document* miss that names the tag asked for and
# the tags held — the next plan's diff — and `admin air-gap-missing` prints
# both columns. mise then pages the listing, which names only what is held,
# and stops.
heavy_mark "mise-unheld"
UNHELDTOOL="$HEAVY_WORK/mise-unheld"
mkdir -p "$UNHELDTOOL"
cat > "$UNHELDTOOL/mise.toml" <<EOF
[tools]
"$TOOL" = { version = "$TOOL_UNHELD_VERSION", exe = "gh" }
EOF
heavy_log "mise install $TOOL@$TOOL_UNHELD_VERSION — a tag the instance does not hold"
run_client "mise install (unheld tag)" "$HEAVY_WORK/mise-unheld.txt" \
  bash -c "cd '$UNHELDTOOL' && ${MISE[*]} install"
[[ $CLIENT_RC -ne 0 ]] || heavy_fail "mise install $TOOL@$TOOL_UNHELD_VERSION succeeded — a listing named a tag the instance does not hold"
heavy_wire_re_after "mise-unheld" "GET /proxy/$GH_REG/$OWNER_REPO/releases/tags/v$TOOL_UNHELD_VERSION -> 503" \
  "the unheld release by tag was not the 503 of RFC 0008"
if heavy_wire_seen_after "mise-unheld" "/releases/download/v$TOOL_UNHELD_VERSION/"; then
  heavy_fail "mise asked for an asset of the unheld tag — something named it"
fi
BATLEHUB_SERVER="$AG_BASE" "$CLI" admin air-gap-missing --registry "$GH_REG" \
  >"$HEAVY_WORK/missing-cli.txt" 2>&1 \
  || { cat "$HEAVY_WORK/missing-cli.txt" >&2; heavy_fail "admin air-gap-missing failed"; }
grep -q "v$TOOL_UNHELD_VERSION" "$HEAVY_WORK/missing-cli.txt" \
  || { cat "$HEAVY_WORK/missing-cli.txt" >&2; heavy_fail "the CLI listing does not name the tag that was asked for"; }
grep -q "v$TOOL_VERSION" "$HEAVY_WORK/missing-cli.txt" \
  || { cat "$HEAVY_WORK/missing-cli.txt" >&2; heavy_fail "the CLI listing does not name the tag that is held"; }
grep -q "Requested" "$HEAVY_WORK/missing-cli.txt" && grep -q "Held" "$HEAVY_WORK/missing-cli.txt" \
  || { cat "$HEAVY_WORK/missing-cli.txt" >&2; heavy_fail "the CLI listing lacks the Requested/Held columns"; }
measure "mise | install $TOOL@$TOOL_UNHELD_VERSION (unheld tag), synthesis on | GET /releases/tags/v$TOOL_UNHELD_VERSION -> 503 (document miss: requested v$TOOL_UNHELD_VERSION, held v$TOOL_VERSION), no asset asked | exit $CLIENT_RC after ${CLIENT_SECS}s | $(grep -E 'mise ERROR Failed' "$HEAVY_WORK/mise-unheld.txt" | head -1 | cut -c1-140)"
heavy_log "MISE-UNHELD-OK (the miss names v$TOOL_UNHELD_VERSION as requested and v$TOOL_VERSION as held)"


# ── 7b. Phase 3: cargo, go, mvn and dotnet resolve through composed listings ─
#
# Each client has its own listing document, composed from the one held
# version (RFC 0008-bis §4.3), and each resolves a *range* or an unpinned
# request against it — the case a lock does not cover.

heavy_mark "cargo-synth"
CARGO_DIR="$HEAVY_WORK/cargo-consumer"
mkdir -p "$CARGO_DIR/src" "$CARGO_DIR/.cargo" "$HEAVY_WORK/cargo-home"
cat >"$CARGO_DIR/Cargo.toml" <<EOF
[package]
name = "heavy-consumer"
version = "0.0.0"
edition = "2021"

[dependencies]
$CRATE = "${CRATE_VERSION%.*}"
EOF
echo 'fn main() {}' >"$CARGO_DIR/src/main.rs"
cat >"$CARGO_DIR/.cargo/config.toml" <<EOF
[source.crates-io]
replace-with = "airgap"

[source.airgap]
registry = "sparse+$HEAVY_TAP_BASE/proxy/$CARGO_REG/registry/"
EOF
heavy_log "cargo generate-lockfile + fetch of $CRATE ${CRATE_VERSION%.*} against the composed sparse index"
run_client "cargo" "$HEAVY_WORK/cargo-synth.txt" \
  bash -c "cd '$CARGO_DIR' && CARGO_HOME='$HEAVY_WORK/cargo-home' CARGO_NET_RETRY=0 cargo generate-lockfile && CARGO_HOME='$HEAVY_WORK/cargo-home' cargo fetch"
[[ $CLIENT_RC -eq 0 ]] || { cat "$HEAVY_WORK/cargo-synth.txt" >&2; heavy_fail "cargo could not resolve $CRATE through the composed sparse index"; }
heavy_wire_re_after "cargo-synth" "GET /proxy/$CARGO_REG/registry/[a-z0-9/]*$CRATE -> 200 .*X-BatleHub-Listing: synthesised" \
  "the sparse index line was not answered as synthesised"
heavy_wire_re_after "cargo-synth" "GET /proxy/$CARGO_REG/$CRATE/$CRATE_VERSION/download -> 200" \
  "cargo did not fetch the crate the composed index named"
grep -q "name = \"$CRATE\"" "$CARGO_DIR/Cargo.lock" && grep -q "version = \"$CRATE_VERSION\"" "$CARGO_DIR/Cargo.lock" \
  || { cat "$CARGO_DIR/Cargo.lock" >&2; heavy_fail "Cargo.lock does not pin $CRATE $CRATE_VERSION"; }
measure "cargo | generate-lockfile + fetch, $CRATE = \"${CRATE_VERSION%.*}\", synthesis on | sparse index -> 200 synthesised (deps from the crate's manifest, read at import), then the crate -> 200 | exit 0 after ${CLIENT_SECS}s | Cargo.lock pins $CRATE_VERSION"
heavy_log "CARGO-SYNTH-OK (a range resolved through a sparse index this instance composed)"

heavy_mark "go-synth"
GO_DIR="$HEAVY_WORK/go-consumer"
mkdir -p "$GO_DIR" "$HEAVY_WORK/go-cache" "$HEAVY_WORK/go-modcache"
cat >"$GO_DIR/go.mod" <<EOF
module example.com/heavy

go 1.22
EOF
heavy_log "go get $GO_MODULE (no version) against the composed @v/list"
# GOSUMDB=off: a checksum database is a service off the site, and an
# air-gapped estate turns it off or mirrors it (RFC 0008 §2). GOFLAGS=-mod=mod
# so the module cache is written, not just read.
run_client "go get" "$HEAVY_WORK/go-synth.txt" \
  env GOPROXY="$HEAVY_TAP_BASE/proxy/$GO_REG" GOSUMDB=off GONOSUMDB="*" GOFLAGS="-mod=mod -modcacherw" \
      GOTOOLCHAIN=local GOCACHE="$HEAVY_WORK/go-cache" GOMODCACHE="$HEAVY_WORK/go-modcache" GOPATH="$HEAVY_WORK/gopath" \
      bash -c "cd '$GO_DIR' && go get $GO_MODULE"
[[ $CLIENT_RC -eq 0 ]] || { cat "$HEAVY_WORK/go-synth.txt" >&2; heavy_fail "go get could not resolve $GO_MODULE through the composed @v/list"; }
heavy_wire_re_after "go-synth" "GET /proxy/$GO_REG/$GO_MODULE/@v/list -> 200 .*X-BatleHub-Listing: synthesised" \
  "@v/list was not answered as synthesised"
heavy_wire_re_after "go-synth" "GET /proxy/$GO_REG/$GO_MODULE/@v/$GO_VERSION.info -> 200 .*X-BatleHub-Listing: synthesised" \
  "the .info was not composed"
heavy_wire_re_after "go-synth" "GET /proxy/$GO_REG/$GO_MODULE/@v/$GO_VERSION.zip -> 200" \
  "go did not fetch the zip the composed list named"
grep -q "$GO_MODULE $GO_VERSION" "$GO_DIR/go.mod" || { cat "$GO_DIR/go.mod" >&2; heavy_fail "go.mod does not require $GO_MODULE $GO_VERSION"; }
measure "go   | go get $GO_MODULE (unpinned), synthesis on | @v/list -> 200 synthesised, .info -> 200 synthesised, then .mod and .zip -> 200 | exit 0 after ${CLIENT_SECS}s | go.mod requires $GO_VERSION"
heavy_log "GO-SYNTH-OK (an unpinned get resolved through a list this instance composed)"

heavy_mark "mvn-synth"
MVN_DIR="$HEAVY_WORK/mvn-consumer"
mkdir -p "$MVN_DIR"
cp -r "$HEAVY_WORK/mvn-warm" "$HEAVY_WORK/mvn-repo"
# Only the version under test and the artifact's own metadata go: the
# dependency plugin depends on other versions of this very artifact, and
# those stay warm — the range must be resolved from the composed metadata,
# not from a cached one.
rm -rf "$HEAVY_WORK/mvn-repo/$MVN_GROUP_PATH/$MVN_ARTIFACT/$MVN_VERSION"
rm -f "$HEAVY_WORK/mvn-repo/$MVN_GROUP_PATH/$MVN_ARTIFACT"/maven-metadata* \
      "$HEAVY_WORK/mvn-repo/$MVN_GROUP_PATH/$MVN_ARTIFACT"/resolver-status.properties
cat >"$HEAVY_WORK/settings-disconnected.xml" <<EOF
<settings>
  <mirrors>
    <mirror>
      <id>airgap</id>
      <mirrorOf>*</mirrorOf>
      <url>$HEAVY_TAP_BASE/proxy/$MVN_REG/maven2</url>
    </mirror>
  </mirrors>
</settings>
EOF
cat >"$MVN_DIR/pom.xml" <<EOF
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>example</groupId>
  <artifactId>consumer</artifactId>
  <version>0.0.0</version>
  <dependencies>
    <dependency>
      <groupId>$MVN_GROUP</groupId>
      <artifactId>$MVN_ARTIFACT</artifactId>
      <version>[${MVN_VERSION%.*},)</version>
    </dependency>
  </dependencies>
</project>
EOF
heavy_log "mvn dependency:resolve of $MVN_ARTIFACT [${MVN_VERSION%.*},) against the composed maven-metadata.xml"
run_client "mvn" "$HEAVY_WORK/mvn-synth.txt" \
  env -u MISE_DATA_DIR -u MISE_CACHE_DIR -u MISE_CONFIG_DIR -u MISE_STATE_DIR \
      bash -c "cd '$MVN_DIR' && ${MVN[*]} -B -s '$HEAVY_WORK/settings-disconnected.xml' -Dmaven.repo.local='$HEAVY_WORK/mvn-repo' dependency:resolve"
[[ $CLIENT_RC -eq 0 ]] || { tail -40 "$HEAVY_WORK/mvn-synth.txt" >&2; heavy_fail "mvn could not resolve the range through the composed metadata"; }
heavy_wire_re_after "mvn-synth" "GET /proxy/$MVN_REG/maven2/$MVN_GROUP_PATH/$MVN_ARTIFACT/maven-metadata.xml -> 200 .*X-BatleHub-Listing: synthesised" \
  "maven-metadata.xml was not answered as synthesised"
heavy_wire_re_after "mvn-synth" "GET /proxy/$MVN_REG/maven2/$MVN_GROUP_PATH/$MVN_ARTIFACT/$MVN_VERSION/$MVN_ARTIFACT-$MVN_VERSION.jar -> 200" \
  "mvn did not fetch the jar the composed metadata named"
# The checksum Maven asks for beside the composed document answers (RFC
# 0008-bis §13.5): one request, not minutes of retried 503s.
heavy_wire_re_after "mvn-synth" "GET /proxy/$MVN_REG/maven2/$MVN_GROUP_PATH/$MVN_ARTIFACT/maven-metadata.xml.sha1 -> 200" \
  "the composed metadata's .sha1 was not answered"
measure "mvn  | dependency:resolve, [${MVN_VERSION%.*},), synthesis on | maven-metadata.xml -> 200 synthesised, then the pom and jar -> 200 | exit 0 after ${CLIENT_SECS}s | resolved $MVN_VERSION"
heavy_log "MVN-SYNTH-OK (a range resolved through metadata this instance composed)"

heavy_mark "dotnet-synth"
DOTNET_SDK="$("${DOTNET[@]}" --version 2>/dev/null | head -1)"
TFM="net${DOTNET_SDK%%.*}.0"
NET_DIR="$HEAVY_WORK/dotnet-consumer"
mkdir -p "$NET_DIR"
# NuGet refuses a plain-http source unless the config says so (NU1302); the
# tap is http, as it is in nuget.sh.
cat >"$NET_DIR/nuget.config" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <clear />
    <add key="airgap" value="$HEAVY_TAP_BASE/proxy/$NUGET_REG/nuget/v3/index.json" allowInsecureConnections="true" />
  </packageSources>
</configuration>
EOF
cat >"$NET_DIR/consumer.csproj" <<EOF
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>$TFM</TargetFramework>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="$NUGET_PKG" Version="${NUGET_VERSION%%.*}.*" />
  </ItemGroup>
</Project>
EOF
heavy_log "dotnet restore of $NUGET_PKG ${NUGET_VERSION%%.*}.* against the composed flat index"
run_client "dotnet restore" "$HEAVY_WORK/dotnet-synth.txt" \
  env -u MISE_DATA_DIR -u MISE_CACHE_DIR -u MISE_CONFIG_DIR -u MISE_STATE_DIR \
      NUGET_PACKAGES="$HEAVY_WORK/nuget-packages" DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 DOTNET_CLI_HOME="$HEAVY_WORK/dotnet-home" \
      bash -c "cd '$NET_DIR' && ${DOTNET[*]} restore"
[[ $CLIENT_RC -eq 0 ]] || { tail -40 "$HEAVY_WORK/dotnet-synth.txt" >&2; heavy_fail "dotnet restore could not resolve the floating version through the composed flat index"; }
heavy_wire_re_after "dotnet-synth" "GET /proxy/$NUGET_REG/nuget/v3/flat/$NUGET_PKG/index.json -> 200 .*X-BatleHub-Listing: synthesised" \
  "the flat index was not answered as synthesised"
heavy_wire_re_after "dotnet-synth" "GET /proxy/$NUGET_REG/nuget/v3/flat/$NUGET_PKG/$NUGET_VERSION/$NUGET_PKG.$NUGET_VERSION.nupkg -> 200" \
  "dotnet did not fetch the package the composed index named"
measure "dotnet | restore, $NUGET_PKG ${NUGET_VERSION%%.*}.*, synthesis on | flat index -> 200 synthesised, then the nupkg -> 200 | exit 0 after ${CLIENT_SECS}s | restored $NUGET_VERSION"
heavy_log "DOTNET-SYNTH-OK (a floating version resolved through a flat index this instance composed)"


# ── 7c. The renderers that open the artifact at import (0008-bis §13.6) ─────
#
# Bundler resolves from the compact index, whose per-gem lines carry the
# dependencies read off the gemspec inside the `.gem`; micromamba resolves
# from `repodata.json`, whose entries are each package's `info/index.json`.
# Both were read at import and are composed here.

heavy_mark "bundle-synth"
GEM_DIR="$HEAVY_WORK/gem-consumer"
mkdir -p "$GEM_DIR"
cat >"$GEM_DIR/Gemfile" <<EOF
source "$HEAVY_TAP_BASE/proxy/$GEMS_REG"
gem "$GEM"
EOF
heavy_log "bundle install of $GEM against the composed compact index"
run_client "bundle install" "$HEAVY_WORK/bundle-synth.txt" \
  env -u MISE_DATA_DIR -u MISE_CACHE_DIR -u MISE_CONFIG_DIR -u MISE_STATE_DIR \
      BUNDLE_USER_HOME="$HEAVY_WORK/bundle-home" BUNDLE_PATH="$GEM_DIR/vendor" BUNDLE_DISABLE_VERSION_CHECK=1 \
      bash -c "cd '$GEM_DIR' && ${BUNDLE[*]} install"
[[ $CLIENT_RC -eq 0 ]] || { tail -40 "$HEAVY_WORK/bundle-synth.txt" >&2; heavy_fail "bundle install could not resolve $GEM through the composed compact index"; }
heavy_wire_re_after "bundle-synth" "GET /proxy/$GEMS_REG/versions -> 200 .*X-BatleHub-Listing: synthesised" \
  "the compact /versions was not answered as synthesised"
heavy_wire_re_after "bundle-synth" "GET /proxy/$GEMS_REG/info/$GEM -> 200 .*X-BatleHub-Listing: synthesised" \
  "the compact /info/$GEM was not answered as synthesised"
heavy_wire_re_after "bundle-synth" "GET /proxy/$GEMS_REG/gems/$GEM-$GEM_VERSION.gem -> 200" \
  "bundler did not fetch the gem the composed index named"
grep -q "$GEM ($GEM_VERSION)" "$GEM_DIR/Gemfile.lock" || { cat "$GEM_DIR/Gemfile.lock" >&2; heavy_fail "Gemfile.lock does not pin $GEM $GEM_VERSION"; }
measure "bundle | install, gem \"$GEM\", synthesis on | /versions and /info/$GEM -> 200 synthesised (deps from the gemspec, read at import), then the gem -> 200 | exit 0 after ${CLIENT_SECS}s | Gemfile.lock pins $GEM_VERSION"
heavy_log "BUNDLE-SYNTH-OK (an unpinned gem resolved through a compact index this instance composed)"

heavy_mark "conda-synth"
CONDA_ROOT="$HEAVY_WORK/mamba-root"
mkdir -p "$CONDA_ROOT"
heavy_log "micromamba create with $CONDA_PKG against the composed repodata.json"
run_client "micromamba create" "$HEAVY_WORK/conda-synth.txt" \
  env MAMBA_ROOT_PREFIX="$CONDA_ROOT" "$MM" create -y --no-rc --override-channels \
      -c "$HEAVY_TAP_BASE/proxy/$CONDA_REG" --platform "$CONDA_SUBDIR" -n probe "$CONDA_PKG"
[[ $CLIENT_RC -eq 0 ]] || { tail -40 "$HEAVY_WORK/conda-synth.txt" >&2; heavy_fail "micromamba could not install $CONDA_PKG through the composed repodata"; }
heavy_wire_re_after "conda-synth" "GET /proxy/$CONDA_REG/$CONDA_SUBDIR/repodata.json[^ ]* -> 200 .*X-BatleHub-Listing: synthesised" \
  "the subdir's repodata was not answered as synthesised"
heavy_wire_re_after "conda-synth" "GET /proxy/$CONDA_REG/noarch/repodata.json[^ ]* -> 200 .*X-BatleHub-Listing: synthesised" \
  "the empty noarch repodata was not answered as synthesised"
heavy_wire_re_after "conda-synth" "GET /proxy/$CONDA_REG/$CONDA_SUBDIR/$CONDA_FILE -> 200" \
  "micromamba did not fetch the package the composed repodata named"
measure "conda | micromamba create $CONDA_PKG, synthesis on | $CONDA_SUBDIR and noarch repodata -> 200 synthesised (entries from info/index.json, read at import), then the package -> 200 | exit 0 after ${CLIENT_SECS}s | installed"
heavy_log "CONDA-SYNTH-OK (a package resolved through a repodata this instance composed)"

# ── 7d. Terraform: the download document, composed and verifiable (§13.7) ──
#
# `terraform init` resolves through the `versions` listing, reads the
# download document for its platform, fetches the archive, the checksum
# list and the list's signature the document names, verifies the signature
# against the document's keys and the archive against the list. Every one
# of those is composed or served here: the keys crossed the gap as facts
# on the manifest, the list and the signature as artifacts, and this
# instance signed nothing.

heavy_mark "terraform-synth"
TF_PROJECT="$HEAVY_WORK/tf-project"
mkdir -p "$TF_PROJECT" "$HEAVY_WORK/tf-plugin-cache"
cat >"$TF_PROJECT/main.tf" <<EOF
terraform {
  required_providers {
    probe = {
      source  = "$TF_HOST/$TF_NS/$TF_TYPE"
      version = "$TF_PROVIDER_VERSION"
    }
  }
}
EOF
cat >"$HEAVY_WORK/terraformrc" <<EOF
plugin_cache_dir = "$HEAVY_WORK/tf-plugin-cache"
disable_checkpoint = true
EOF
heavy_log "terraform init ($TF_PROVIDER $TF_PROVIDER_VERSION through $TF_HOST, composed listing and download document)"
run_client "terraform init" "$HEAVY_WORK/terraform-synth.txt" \
  env -u MISE_DATA_DIR -u MISE_CACHE_DIR -u MISE_CONFIG_DIR -u MISE_STATE_DIR \
      SSL_CERT_FILE="$TF_CERT" TF_CLI_CONFIG_FILE="$HEAVY_WORK/terraformrc" TF_IN_AUTOMATION=1 CHECKPOINT_DISABLE=1 \
      bash -c "cd '$TF_PROJECT' && ${TF[*]} init -no-color"
[[ $CLIENT_RC -eq 0 ]] || { tail -40 "$HEAVY_WORK/terraform-synth.txt" >&2; heavy_fail "terraform init could not install $TF_PROVIDER through the composed download document"; }
grep -q "Terraform has been successfully initialized" "$HEAVY_WORK/terraform-synth.txt" \
  || heavy_fail "terraform init reported no success line"
grep -qi "signed by" "$HEAVY_WORK/terraform-synth.txt" \
  || { tail -20 "$HEAVY_WORK/terraform-synth.txt" >&2; heavy_fail "Terraform did not report a signature verification — the carried keys, the held list and its signature are what make that possible"; }
TF_BASE="/v1/providers/$TF_NS/$TF_TYPE"
heavy_wire_after "terraform-synth" "GET /.well-known/terraform.json -> 200" \
  "discovery did not answer on Terraform's host"
heavy_wire_re_after "terraform-synth" "GET $TF_BASE/versions -> 200 .*X-BatleHub-Listing: synthesised" \
  "the versions listing was not answered as synthesised"
heavy_wire_re_after "terraform-synth" "GET $TF_BASE/$TF_PROVIDER_VERSION/download/linux/amd64 -> 200 .*X-BatleHub-Listing: synthesised" \
  "the download document was not answered as synthesised"
heavy_wire_after "terraform-synth" "$TF_BASE/$TF_PROVIDER_VERSION/shasums -> 200" \
  "the checksum list was not served from the held set"
heavy_wire_after "terraform-synth" "$TF_BASE/$TF_PROVIDER_VERSION/shasums.sig -> 200" \
  "the signature was not served from the held set"
heavy_wire_after "terraform-synth" "$TF_BASE/$TF_PROVIDER_VERSION/artifact/linux/amd64 -> 200" \
  "the archive was not served from the held set"
measure "terraform | init, $TF_PROVIDER $TF_PROVIDER_VERSION, synthesis on | versions and download document -> 200 synthesised (keys carried on the manifest), then shasums, shasums.sig and the archive -> 200 | exit 0 after ${CLIENT_SECS}s | initialized, $(grep -i "signed by" "$HEAVY_WORK/terraform-synth.txt" | head -1 | sed 's/^ *//')"
heavy_log "TERRAFORM-SYNTH-OK (a provider installed and verified through a download document this instance composed)"

# ── 8. The miss log, after the second half ──────────────────────────────────
#
# Purged before the flip, so this is what the synthesised half left: none of
# the three listings, nothing the bundle carried, and pip's self-check.
# Scoped to this run's registries: the heavy suites share one database, so
# the unfiltered list is every run's. The carried keys are read off the
# bundle's own manifest, so "nothing the bundle carried is missing" is exact.
for r in "$NPM_REG" "$PIP_REG" "$GH_REG" "$CARGO_REG" "$GO_REG" "$MVN_REG" "$NUGET_REG" "$GEMS_REG" "$CONDA_REG" "$TF_REG"; do
  curl -s -H "Authorization: Bearer $ADMIN_TOKEN" "$AG_BASE/api/v1/admin/air-gap/missing?registry=$r&per_page=100" \
    > "$HEAVY_WORK/missing-$r.json"
done
python3 - "$HEAVY_WORK" "$HEAVY_WORK/estate.bhub" "$NPM_REG" "$NPM_PKG" "$PIP_REG" "$PIP_PKG" "$GH_REG" "$OWNER_REPO" "$TOOL_VERSION" "$TOOL_UNHELD_VERSION" "$MEASURE_FILE" "$CARGO_REG" "$GO_REG" "$MVN_REG" "$NUGET_REG" "$GO_MODULE" "$GEMS_REG" "$CONDA_REG" "$TF_REG" <<'PY' || heavy_fail "the miss log does not say what the clients asked for"
import json, sys, tarfile
work, bundle, npm_reg, npm_pkg, pip_reg, pip_pkg, gh_reg, repo, tool_v, tool_unheld, measure, cargo_reg, go_reg, mvn_reg, nuget_reg, go_module, gems_reg, conda_reg, tf_reg = sys.argv[1:]
with tarfile.open(bundle) as t:
    carried = {(e["registry"], e["key"]) for e in json.load(t.extractfile("manifest.json"))["entries"]}
rows = {}
for r in (npm_reg, pip_reg, gh_reg, cargo_reg, go_reg, mvn_reg, nuget_reg, gems_reg, conda_reg, tf_reg):
    d = json.load(open(f"{work}/missing-{r}.json"))
    assert d["air_gapped"] is True, d
    for i in d["items"]:
        rows[(i["registry"], i["kind"], i["storage_key"])] = i
print("miss log:", len(rows), "row(s):", sorted(f"{r}:{kind}:{k}" for (r, kind, k) in rows))
docs = {(r, k) for (r, kind, k) in rows if kind == "document"}
assert not any(r == npm_reg and k.startswith(npm_pkg) for (r, k) in docs), f"the synthesised packument was recorded as a miss: {docs}"
assert not any(r == pip_reg and k.startswith(pip_pkg) for (r, k) in docs), f"the synthesised simple page was recorded as a miss: {docs}"
gh_docs = {k for (r, k) in docs if r == gh_reg}
assert gh_docs == {f"{repo} (release)"}, f"the forge's document misses should be the one unheld release by tag: {gh_docs}"
unheld = rows[(gh_reg, "document", f"{repo} (release)")]
assert unheld.get("requested_version") == f"v{tool_unheld}", unheld
assert f"v{tool_v}" in unheld.get("held_versions", []), unheld
assert any(r == pip_reg and k == "pip (simple-json)" for (r, k) in docs), f"pip's self-check is the one document miss expected: {docs}"
# What the four clients asked for beyond their composed listings, all of it
# measured and all of it honest (0008-bis §13.4): `go get` probes the
# parent module paths (`github.com`, `github.com/google`) before it settles
# on the module, and Maven asks for `.sha1`/`.md5` beside every file, the
# composed metadata included. Nothing else of theirs may be missing.
def expected_phase3(r, kind, k):
    if r == go_reg and kind == "document":
        return any(k == f"{p} (versions)" for p in ("/".join(module_parts[:i]) for i in range(1, len(module_parts))))
    if r == mvn_reg and kind == "artifact":
        return k.endswith(".sha1") or k.endswith(".md5")
    return False
module_parts = go_module.split("/")
phase3 = sorted(f"{r}:{kind}:{k}" for (r, kind, k) in rows
                if r in (cargo_reg, go_reg, mvn_reg, nuget_reg, gems_reg, conda_reg, tf_reg) and not expected_phase3(r, kind, k))
print("misses of the artifact-parsed kinds:", sorted(f"{r}:{kind}:{k}" for (r, kind, k) in rows if r in (gems_reg, conda_reg, tf_reg)))
assert not phase3, f"cargo, go, mvn, dotnet, bundler, micromamba and terraform resolved through composed listings, so nothing of theirs is missing but the probes and checksum files: {phase3}"
assert not any(r == gh_reg and k.endswith(f"{repo}/v{tool_v}") for (r, kind, k) in rows), f"the composed release by tag was recorded as a miss: {list(rows)}"
missing_carried = [(r, k) for (r, kind, k) in rows if (r, k) in carried]
assert not missing_carried, f"a key the bundle carried was recorded as missing: {missing_carried}"
with open(measure, "a") as f:
    for (r, kind, k), i in sorted(rows.items()):
        f.write(f"miss | {r} | {kind} | {k} | requested={i.get('requested_version') or '-'} held={','.join(i.get('held_versions') or []) or '-'} | requests={i.get('count')}\n")
PY
heavy_log "AIRGAP-MISSLOG-OK (after the synthesised half: no listing missing, nothing carried missing, pip's self-check the one row)"

heavy_log "Measurement (RFC 0008-bis phase 0), npm $(npm --version), pip $PIP_VERSION_STR, $("${MISE[@]}" --version 2>/dev/null | head -1), $("${TF[@]}" version 2>/dev/null | head -1)"
cat "$MEASURE_FILE"

heavy_stop_second_server
heavy_done AIRGAP-HEAVY-OK
