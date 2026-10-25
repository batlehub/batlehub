#!/usr/bin/env bash
# Heavy `setup apply` suite — RFC 0038 §6.5.
#
# `batlehub-cli setup apply` writes each client's user-level configuration from
# the server's registry list. A unit test can prove the bytes it writes; only
# the clients can prove those bytes are read. So every client here runs with
# nothing configured but what `apply` wrote, and with the world closed the way
# `closed_world.sh` closes it — `closed_proxy.py` relays the loopback and
# refuses every other host — so an install that succeeds got its packages from
# this instance and could have got them from nowhere else.
#
# Each client runs under `env -i` with its own `HOME`: a variable inherited
# from the runner (`GOPROXY`, `NPM_CONFIG_REGISTRY`) would configure the client
# behind `apply`'s back, and a phase would pass for the wrong reason. `PATH` is
# a directory of links to the real client binaries, resolved before `HOME`
# moves — a mise shim stops resolving the moment it does.
#
#   1. `setup apply` without `--yes` exits 0 and writes no file.
#   2. `setup apply --yes`, then one install per client — cargo, npm, pnpm,
#      yarn 4, pip, uv, go, mvn, mise (a GitHub release, and helm through a
#      generic mirror), rustup — each succeeds with egress
#      denied, and each artifact request is on the tap. go also proves RFC
#      0038 §4.2's rule: with `GOSUMDB` unset, its checksum lookups come
#      through `/proxy/<go>/sumdb/`.
#   3. A second `apply --yes` moves no file.
#   4. A user `registry=` in `~/.npmrc`: `conflict`, exit 3, the file
#      byte-identical, and npm refused by the closed world.
#   5. A user `[global]` in `pip.conf`: `conflict`, untouched, still parsed.
#   6. `GOPROXY` in the environment: `conflict`, named, though the file is
#      `absent`.
#   7. `setup remove --yes`: every file byte-identical to its pre-`apply` copy,
#      one seeded without a final newline; the files `apply` created gone.
#   8. A second npm *proxy* registry and a *local* one added by reload: npm is
#      `ambiguous` between the two proxies only (§4.4 — a local registry is not
#      a candidate); `--pick npm=<b>` writes b; a plain `apply --yes`
#      keeps it, and keeps `--maven-mirror-of all`.
#   9. The pypi registry removed by reload: pip is `orphaned`, `apply --yes`
#      keeps the block (fail closed, exit 3), and `remove --yes` takes it out.
#
# bun is not driven: it would be one more client download for the same
# `~/.npmrc` block npm and pnpm already prove. node's `NODEJS_ORG_MIRROR` is
# asserted in `env.sh` and driven by `closed_world.sh` §11 (nvm).
#
# Run via `task test:client-setup-heavy` or directly. Needs DATABASE_URL,
# network for the server, and cargo, npm, node, python3, go, mvn, mise, uv and
# rustup — the ones that are mise-managed here are resolved through it.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init client_setup 8180 8181
heavy_need curl "curl"
heavy_need python3 "python3 (the wire tap)"

PNPM_VERSION="${HEAVY_CS_PNPM:-11.26.0}"
YARN_VERSION="${HEAVY_CS_YARN:-4.18.1}"
JAVA_VERSION="${HEAVY_CS_JAVA:-temurin-21.0.12+101.0.LTS}"
MAVEN_VERSION="${HEAVY_CS_MAVEN:-3.9.16}"
MISE_TOOL="${HEAVY_CS_MISE_TOOL:-github:cli/cli}"
MISE_TOOL_VERSION="${HEAVY_CS_MISE_TOOL_VERSION:-2.60.0}"
RUST_VERSION="${HEAVY_CS_RUST:-1.85.0}"

CRATES="crates-$HEAVY_RUN"
NPM="npm-$HEAVY_RUN"
PYPI="pypi-$HEAVY_RUN"
GO="go-$HEAVY_RUN"
MAVEN="maven-$HEAVY_RUN"
GITHUB="github-$HEAVY_RUN"
HELM="helm-$HEAVY_RUN"
HELM_VERSION="${HEAVY_CS_HELM:-3.16.4}"
RUST="rust-$HEAVY_RUN"
NODEDIST="nodedist-$HEAVY_RUN"
NPM_B="npmb-$HEAVY_RUN"
NPM_LOCAL="npmlocal-$HEAVY_RUN"

# ── The clients, resolved before HOME moves ──────────────────────────────────

heavy_log "Building batlehub-cli"
cargo build -p batlehub-cli >"$HEAVY_WORK/cli-build.log" 2>&1 \
  || { cat "$HEAVY_WORK/cli-build.log" >&2; heavy_fail "batlehub-cli did not build"; }
CLI="$HEAVY_ROOT/target/debug/batlehub-cli"

TOOLS="$HEAVY_WORK/tools"
mkdir -p "$TOOLS"

# link_tool <bin> [mise-spec...] — link a working <bin> into $TOOLS. A mise
# shim answers `command -v` and then refuses to run without its config, so the
# test is running it; `mise which` turns a shim into the binary it stands for.
link_tool() {
  local bin="$1"
  shift
  local p=""
  p="$(mise which "$bin" 2>/dev/null || command -v "$bin" 2>/dev/null || true)"
  if [[ -z "$p" ]] || ! { "$p" --version || "$p" version; } >/dev/null 2>&1; then
    [[ $# -gt 0 ]] && command -v mise >/dev/null 2>&1 \
      || heavy_fail "no working $bin on this machine"
    p="$(mise x "$@" -- bash -c "command -v $bin" 2>/dev/null)" \
      || heavy_fail "no working $bin, and mise could not provide $*"
  fi
  wrap_tool "$bin" "$p"
}

# wrap_tool <name> <path> — a wrapper rather than a symlink: npm's launcher
# finds `node` relative to the path it was invoked by, which a link would make
# $TOOLS. `exec` keeps argv[0] the real path, which rustup's proxies key on.
wrap_tool() {
  printf '#!/bin/sh\nexec %q "$@"\n' "$2" > "$TOOLS/$1"
  chmod +x "$TOOLS/$1"
}

link_tool node
link_tool npm
link_tool python3
link_tool cargo
link_tool rustc
link_tool rustup
link_tool go
link_tool uv
link_tool mise
link_tool java "java@$JAVA_VERSION"
link_tool mvn "maven@$MAVEN_VERSION" "java@$JAVA_VERSION"
RUSTUP_REAL_HOME="$(rustup show home 2>/dev/null || echo "$HOME/.rustup")"

# pip from a venv the suite owns, so `pip install` cannot touch the runner's
# Python. Offline: ensurepip ships its own wheel.
PY_VENV="$HEAVY_WORK/py-venv"
"$(mise which python3 2>/dev/null || command -v python3)" -m venv "$PY_VENV" || heavy_fail "python3 -m venv failed"
wrap_tool pip "$PY_VENV/bin/pip"

# pnpm and yarn 4, from npmjs with the world still open: the clients under
# test, not anything this instance is asked for.
heavy_log "Installing pnpm $PNPM_VERSION and yarn $YARN_VERSION into the run's directory"
heavy_retry 3 "installing pnpm and yarn" \
  "$TOOLS/npm" install --prefix "$HEAVY_WORK/npm-tools" --no-audit --no-fund \
  --ignore-scripts "pnpm@$PNPM_VERSION" "@yarnpkg/cli-dist@$YARN_VERSION" \
  >"$HEAVY_WORK/npm-tools.log" 2>&1 \
  || { cat "$HEAVY_WORK/npm-tools.log" >&2; heavy_fail "could not install pnpm and yarn"; }
wrap_tool pnpm "$HEAVY_WORK/npm-tools/node_modules/.bin/pnpm"
wrap_tool yarn "$HEAVY_WORK/npm-tools/node_modules/.bin/yarn"

for t in "$TOOLS"/*; do
  heavy_log "client: $(basename "$t") → $(sed -n 2p "$t")"
done

# ── The server, from a copy the reload cases can edit ────────────────────────

# The forge registry (mise's github) is authenticated when the run has a token:
# anonymous, the GitHub budget is per shared runner IP and regularly spent
# before the mise phase (lib.sh, `heavy_forge_auth_config`). The reload cases
# edit $CONFIG, so it is a copy of whatever that returns.
sed "s/\${HEAVY_RUN}/$HEAVY_RUN/" tests/heavy/config.client-setup.toml > "$HEAVY_WORK/client-setup.src.toml"
heavy_forge_auth_config "$HEAVY_WORK/client-setup.src.toml"
CONFIG="$HEAVY_WORK/config.toml"
cp "$HEAVY_CONFIG" "$CONFIG"
heavy_start_server "$CONFIG"
heavy_start_tap

# ── The closed world, as closed_world.sh builds it ───────────────────────────

CLOSED_PROXY_PORT="${HEAVY_CS_DENY_PORT:-8182}"
CLOSED_PROXY="http://127.0.0.1:$CLOSED_PROXY_PORT"
LOOPBACK_DIRECT="127.0.0.1,localhost"
DENY=(HTTP_PROXY="$CLOSED_PROXY" HTTPS_PROXY="$CLOSED_PROXY"
      http_proxy="$CLOSED_PROXY" https_proxy="$CLOSED_PROXY"
      NO_PROXY="$LOOPBACK_DIRECT" no_proxy="$LOOPBACK_DIRECT")
python3 tests/heavy/closed_proxy.py "$CLOSED_PROXY_PORT" \
  >"$HEAVY_WORK/closed-proxy.err" 2>&1 &
HEAVY_EXTRA_PIDS+=("$!")
for _ in $(seq 1 50); do
  (exec 3<>"/dev/tcp/127.0.0.1/$CLOSED_PROXY_PORT") 2>/dev/null && break
  sleep 0.1
done

# machine <home> — set MENV to the bare environment of a machine whose home is
# <home>: nothing inherited, PATH the linked clients only.
machine() {
  local home="$1"
  mkdir -p "$home"
  MENV=(env -i HOME="$home" USER=heavy SHELL=/bin/bash LANG=C.UTF-8 TERM=dumb
        PATH="$TOOLS:/usr/bin:/bin"
        XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share"
        XDG_CACHE_HOME="$home/.cache" XDG_STATE_HOME="$home/.local/state"
        CARGO_HOME="$home/.cargo" RUSTUP_HOME="$RUSTUP_REAL_HOME")
}

# setup <home> <args...> — `batlehub-cli setup …` on that machine. The exit code
# is in SETUP_CODE and the output in SETUP_OUT; a non-zero code is a result
# here, not a failure, because 3 is half of what this suite asserts.
SETUP_OUT="$HEAVY_WORK/setup.txt"
setup() {
  local home="$1"
  shift
  machine "$home"
  SETUP_CODE=0
  "${MENV[@]}" "$CLI" --server "$HEAVY_TAP_BASE" setup "$@" >"$SETUP_OUT" 2>&1 || SETUP_CODE=$?
}

expect_code() {  # <code> <what>
  [[ "$SETUP_CODE" == "$1" ]] || {
    cat "$SETUP_OUT" >&2
    heavy_fail "$2: exit $SETUP_CODE, expected $1"
  }
}

# client <home> <out> <dir> <cmd...> — a client run on that machine, egress denied.
client() {
  local home="$1" out="$2" dir="$3"
  shift 3
  machine "$home"
  mkdir -p "$dir"
  (cd "$dir" && "${MENV[@]}" "${DENY[@]}" "$@") >"$out" 2>&1
}

heavy_log "Egress control: a client process reaching a real registry"
machine "$HEAVY_WORK/probe"
status="$("${MENV[@]}" "${DENY[@]}" curl -sS --max-time 20 -w '%{http_code}' \
  -o "$HEAVY_WORK/egress.body" https://registry.npmjs.org/left-pad 2>/dev/null)" || status="unreachable"
case "$status" in
  403) grep -q "closed world" "$HEAVY_WORK/egress.body" \
         || heavy_fail "registry.npmjs.org answered 403 from something other than the closed-world proxy" ;;
  unreachable) ;;
  *) heavy_fail "a client process reached registry.npmjs.org ($status) — the world is not closed" ;;
esac

MH="$HEAVY_WORK/machine"
TARGETS=(.npmrc .bashrc .yarnrc.yml .cargo/config.toml .config/pip/pip.conf
         .config/uv/uv.toml .config/go/env .m2/settings.xml
         .config/mise/conf.d/batlehub.toml .config/batlehub/env.sh)
mkdir -p "$MH"
# Seeded: one file without a final newline (the marker's `nl`), one with.
printf 'save-exact=true' > "$MH/.npmrc"
printf "alias ll='ls -l'\n" > "$MH/.bashrc"
BEFORE="$HEAVY_WORK/before"
mkdir -p "$BEFORE"
for f in "${TARGETS[@]}"; do
  [[ -f "$MH/$f" ]] && { mkdir -p "$BEFORE/$(dirname "$f")"; cp -p "$MH/$f" "$BEFORE/$f"; }
done

# ── 1. A dry run writes nothing ──────────────────────────────────────────────

heavy_mark dry-run
heavy_log "1. setup apply, no --yes"
setup "$MH" apply
expect_code 0 "the dry run"
heavy_client_must_say "$SETUP_OUT" "would change; run with --yes" "the dry run printed no diff"
for f in "${TARGETS[@]}"; do
  if [[ -f "$BEFORE/$f" ]]; then
    cmp -s "$BEFORE/$f" "$MH/$f" || heavy_fail "the dry run changed $f"
  else
    [[ ! -e "$MH/$f" ]] || heavy_fail "the dry run created $f"
  fi
done
heavy_wire_after dry-run "GET /api/v1/registries -> 200" "apply did not read the registry list through the tap"

# ── 2. Written, and read by every client ─────────────────────────────────────

heavy_log "2. setup apply --yes"
setup "$MH" apply --yes
expect_code 0 "apply --yes"
cat "$SETUP_OUT"
grep -q "^export RUSTUP_DIST_SERVER='$HEAVY_TAP_BASE/proxy/$RUST/rustup'$" "$MH/.config/batlehub/env.sh" \
  || heavy_fail "env.sh does not point rustup at the instance"
grep -q "^export NODEJS_ORG_MIRROR='$HEAVY_TAP_BASE/proxy/$NODEDIST/nodedist'$" "$MH/.config/batlehub/env.sh" \
  || heavy_fail "env.sh does not point node at the instance"
grep -q "registry=$HEAVY_TAP_BASE/proxy/$NPM/" "$MH/.yarnrc.yml" 2>/dev/null \
  && heavy_fail "yarn 4 got an .npmrc line in .yarnrc.yml"
grep -q "npmRegistryServer: \"$HEAVY_TAP_BASE/proxy/$NPM/\"" "$MH/.yarnrc.yml" \
  || heavy_fail "yarn $YARN_VERSION was not written its own .yarnrc.yml"

OUT="$HEAVY_WORK/client.txt"

heavy_mark cargo
heavy_log "cargo fetch"
mkdir -p "$HEAVY_WORK/cargo/src"
cat > "$HEAVY_WORK/cargo/Cargo.toml" <<'EOF'
[package]
name = "client-setup"
version = "0.1.0"
edition = "2021"

[dependencies]
itoa = "=1.0.11"
EOF
echo 'fn main() {}' > "$HEAVY_WORK/cargo/src/main.rs"
client "$MH" "$OUT" "$HEAVY_WORK/cargo" CARGO_NET_RETRY=1 cargo fetch \
  || { cat "$OUT" >&2; heavy_fail "cargo fetch failed with the configuration apply wrote"; }
heavy_wire_after cargo "GET /proxy/$CRATES/registry/config.json -> 200" \
  "cargo did not read the sparse index through the instance"
heavy_wire_re_after cargo "GET /proxy/$CRATES/itoa/1[.]0[.]11/download -> 200" \
  "the crate did not come through the instance"

PKG='{"name":"client-setup","version":"1.0.0","private":true,"dependencies":{"left-pad":"1.3.0"}}'
for c in npm pnpm yarn; do
  dir="$HEAVY_WORK/$c"
  mkdir -p "$dir"
  echo "$PKG" > "$dir/package.json"
  heavy_mark "$c"
  heavy_log "$c install"
  case "$c" in
    npm)  client "$MH" "$OUT" "$dir" npm_config_cache="$HEAVY_WORK/npm-cache" \
            npm install --no-audit --no-fund ;;
    pnpm) client "$MH" "$OUT" "$dir" pnpm install ;;
    yarn) : > "$dir/yarn.lock"
          client "$MH" "$OUT" "$dir" YARN_ENABLE_TELEMETRY=0 yarn install ;;
  esac || { cat "$OUT" >&2; heavy_fail "$c install failed with the configuration apply wrote"; }
  heavy_wire_after "$c" "GET /proxy/$NPM/left-pad -> 200" "$c did not read the packument through the instance"
  # yarn 4 installs Plug'n'Play into a global cache: its lockfile is the record.
  [[ -f "$dir/node_modules/left-pad/package.json" ]] || grep -q '^"left-pad@' "$dir/yarn.lock" 2>/dev/null \
    || heavy_fail "$c reported success and installed no left-pad"
done

heavy_mark pip
heavy_log "pip install"
client "$MH" "$OUT" "$HEAVY_WORK" PIP_DISABLE_PIP_VERSION_CHECK=1 PIP_NO_CACHE_DIR=1 \
  pip install six==1.16.0 \
  || { cat "$OUT" >&2; heavy_fail "pip install failed with the configuration apply wrote"; }
heavy_wire_re_after pip "GET /proxy/$PYPI/simple/six/? -> 200" "pip did not read the simple page through the instance"

heavy_mark uv
heavy_log "uv pip install"
client "$MH" "$OUT" "$HEAVY_WORK" UV_PYTHON_DOWNLOADS=never \
  uv venv "$HEAVY_WORK/uv-venv" --python "$PY_VENV/bin/python" \
  || { cat "$OUT" >&2; heavy_fail "uv venv failed"; }
client "$MH" "$OUT" "$HEAVY_WORK" UV_PYTHON_DOWNLOADS=never UV_CACHE_DIR="$HEAVY_WORK/uv-cache" \
  uv pip install --python "$HEAVY_WORK/uv-venv" six==1.16.0 \
  || { cat "$OUT" >&2; heavy_fail "uv pip install failed with the configuration apply wrote"; }
heavy_wire_re_after uv "GET /proxy/$PYPI/simple/six/? -> 200" "uv did not read the simple page through the instance"

heavy_mark go
heavy_log "go mod tidy, GOSUMDB left at its default"
mkdir -p "$HEAVY_WORK/go"
cat > "$HEAVY_WORK/go/go.mod" <<'EOF'
module clientsetup

go 1.21

require rsc.io/quote v1.5.2
EOF
cat > "$HEAVY_WORK/go/main.go" <<'EOF'
package main

import (
	"fmt"

	"rsc.io/quote"
)

func main() { fmt.Println(quote.Hello()) }
EOF
client "$MH" "$OUT" "$HEAVY_WORK/go" GOTOOLCHAIN=local GOFLAGS="-modcacherw" \
  GOMODCACHE="$HEAVY_WORK/go-cache/mod" GOCACHE="$HEAVY_WORK/go-cache/build" GOPATH="$HEAVY_WORK/go-cache/path" \
  go mod tidy \
  || { cat "$OUT" >&2; heavy_fail "go mod tidy failed with the configuration apply wrote"; }
heavy_wire_after go "GET /proxy/$GO/rsc.io/quote/@v/v1.5.2.zip -> 200" "the module did not come through the instance"
heavy_wire_after go "GET /proxy/$GO/sumdb/sum.golang.org/supported -> 200" \
  "go did not ask the instance whether it proxies the checksum database"
heavy_wire_re_after go "GET /proxy/$GO/sumdb/sum[.]golang[.]org/lookup/rsc[.]io/quote@v1[.]5[.]2 -> 200" \
  "go verified the module without the instance's sumdb — GOPROXY alone did not carry it"

heavy_mark maven
heavy_log "mvn dependency:resolve"
mkdir -p "$HEAVY_WORK/maven"
cat > "$HEAVY_WORK/maven/pom.xml" <<'EOF'
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>sh.batlehub</groupId>
  <artifactId>client-setup</artifactId>
  <version>1.0.0</version>
  <dependencies>
    <dependency>
      <groupId>org.apache.commons</groupId>
      <artifactId>commons-lang3</artifactId>
      <version>3.19.0</version>
    </dependency>
  </dependencies>
</project>
EOF
# `user.home` is Java's, read from the password database rather than `$HOME`:
# a real login has the two agree, this redirected one does not.
client "$MH" "$OUT" "$HEAVY_WORK/maven" MAVEN_OPTS="-Duser.home=$MH" \
  mvn -B -ntp -Dmaven.repo.local="$HEAVY_WORK/m2" dependency:resolve \
  || { cat "$OUT" >&2; heavy_fail "mvn dependency:resolve failed with the settings.xml apply wrote"; }
heavy_wire_after maven \
  "GET /proxy/$MAVEN/maven2/org/apache/commons/commons-lang3/3.19.0/commons-lang3-3.19.0.jar -> 200" \
  "the jar did not come through the instance's mirror"

heavy_mark mise
heavy_log "mise install $MISE_TOOL@$MISE_TOOL_VERSION"
# Attestation verification goes to api.github.com outside url_replacements
# (closed_world.sh §10); turning it off is the user's choice, not apply's.
client "$MH" "$OUT" "$HEAVY_WORK" MISE_YES=1 MISE_GITHUB_ATTESTATIONS=false \
  mise install "$MISE_TOOL@$MISE_TOOL_VERSION" \
  || { cat "$OUT" >&2; heavy_fail "mise install failed with the drop-in apply wrote"; }
heavy_wire_re_after mise "GET /proxy/$GITHUB/cli/cli/releases/download/[^ ]* -> 200" \
  "the release asset did not come through the instance"

# RFC 0038 phase 2: the generic registry's rule is `^{upstream}/(.+)`, written
# from the `upstreams` the registry list now carries. aqua's helm downloads
# from get.helm.sh, a host no typed rule covers.
heavy_mark mise-generic
heavy_log "mise install aqua:helm/helm@$HELM_VERSION, through the generic mirror"
client "$MH" "$OUT" "$HEAVY_WORK" MISE_YES=1 MISE_GITHUB_ATTESTATIONS=false \
  MISE_AQUA_GITHUB_ATTESTATIONS=false MISE_AQUA_COSIGN=false MISE_AQUA_SLSA=false \
  mise install "aqua:helm/helm@$HELM_VERSION" \
  || { cat "$OUT" >&2; heavy_fail "mise install of helm failed with the generic rule apply wrote"; }
heavy_wire_after mise-generic \
  "GET /proxy/$HELM/generic/helm-v$HELM_VERSION-linux-amd64.tar.gz -> 200" \
  "the helm archive did not come through the generic mirror"

heavy_mark rustup
heavy_log "rustup toolchain install $RUST_VERSION, from an interactive bash that sources ~/.bashrc"
# `bash -i` reads ~/.bashrc, whose block sources env.sh: the whole chain
# `--shell-rc auto` sets up, with nothing exported by this script.
client "$MH" "$OUT" "$HEAVY_WORK" RUSTUP_HOME="$HEAVY_WORK/rustup-home" \
  bash -i -c "rustup toolchain install $RUST_VERSION --profile minimal --no-self-update && rustup run $RUST_VERSION rustc --version" \
  || { cat "$OUT" >&2; heavy_fail "rustup could not install through the variables env.sh exports"; }
heavy_client_must_say "$OUT" "^rustc $RUST_VERSION " "the toolchain that ran is not the one the instance served"
heavy_wire_after rustup "GET /proxy/$RUST/rustup/dist/channel-rust-$RUST_VERSION.toml.sha256 -> 200" \
  "rustup did not read the channel manifest through the instance"

# ── 3. A second apply moves nothing ──────────────────────────────────────────

heavy_log "3. a second setup apply --yes"
stamp() { for f in "${TARGETS[@]}"; do [[ -e "$MH/$f" ]] && stat -c '%n %y' "$MH/$f"; done; return 0; }
stamp > "$HEAVY_WORK/mtimes.before"
setup "$MH" apply --yes
expect_code 0 "the second apply"
heavy_client_must_say "$SETUP_OUT" "Nothing to write" "the second apply wanted to write"
stamp > "$HEAVY_WORK/mtimes.after"
diff "$HEAVY_WORK/mtimes.before" "$HEAVY_WORK/mtimes.after" >&2 \
  || heavy_fail "the second apply moved a file"
setup "$MH" status
expect_code 0 "status after apply"

# ── 4. A user registry= in ~/.npmrc ──────────────────────────────────────────

heavy_log "4. a user registry= line"
M4="$HEAVY_WORK/machine-npmrc"
mkdir -p "$M4"
printf 'registry=https://registry.npmjs.org/\n' > "$M4/.npmrc"
cp "$M4/.npmrc" "$HEAVY_WORK/npmrc.seeded"
setup "$M4" status --only npm
expect_code 3 "status with a user registry="
heavy_client_must_say "$SETUP_OUT" "conflict" "status did not report the conflict"
setup "$M4" apply --yes --only npm
expect_code 3 "apply with a user registry="
cmp -s "$HEAVY_WORK/npmrc.seeded" "$M4/.npmrc" || heavy_fail "apply edited a .npmrc the user owns"
dir="$HEAVY_WORK/npm-conflict"
mkdir -p "$dir"
echo "$PKG" > "$dir/package.json"
if client "$M4" "$OUT" "$dir" npm_config_cache="$HEAVY_WORK/npm-cache-4" npm install --no-audit --no-fund; then
  heavy_fail "npm installed with the user's registry= in a closed world — the conflict was masked"
fi
# Failing is not enough: it must fail *because* the user's registry= sent it to
# registry.npmjs.org and the closed world refused it — not on a cache, a
# parse or a network error that would pass this case for any reason.
heavy_client_must_say "$OUT" "403|closed world" \
  "npm failed, but not on the closed world's 403 for registry.npmjs.org"

# ── 5. A user [global] in pip.conf ───────────────────────────────────────────

heavy_log "5. a user [global] section"
M5="$HEAVY_WORK/machine-pip"
mkdir -p "$M5/.config/pip"
printf '[global]\ntimeout = 30\n' > "$M5/.config/pip/pip.conf"
cp "$M5/.config/pip/pip.conf" "$HEAVY_WORK/pip.seeded"
setup "$M5" apply --yes --only pip
expect_code 3 "apply with a user [global]"
cmp -s "$HEAVY_WORK/pip.seeded" "$M5/.config/pip/pip.conf" || heavy_fail "apply edited a pip.conf the user owns"
client "$M5" "$OUT" "$HEAVY_WORK" pip config list \
  || { cat "$OUT" >&2; heavy_fail "pip can no longer parse its config"; }
heavy_client_must_say "$OUT" "global.timeout='30'" "pip does not read the user's setting any more"

# ── 6. GOPROXY in the environment ────────────────────────────────────────────

heavy_log "6. GOPROXY exported"
M6="$HEAVY_WORK/machine-goproxy"
machine "$M6"
SETUP_CODE=0
"${MENV[@]}" GOPROXY=https://proxy.golang.org "$CLI" --server "$HEAVY_TAP_BASE" setup status --only go \
  >"$SETUP_OUT" 2>&1 || SETUP_CODE=$?
expect_code 3 "status with GOPROXY exported"
heavy_client_must_say "$SETUP_OUT" '^go +conflict' "status did not report go in conflict"
heavy_client_must_say "$SETUP_OUT" '\$GOPROXY is set and overrides' "status did not name the GOPROXY override"

# ── 7. remove gives every file back ──────────────────────────────────────────

heavy_log "7. setup remove --yes"
setup "$MH" remove --yes
expect_code 0 "remove"
for f in "${TARGETS[@]}"; do
  if [[ -f "$BEFORE/$f" ]]; then
    cmp "$BEFORE/$f" "$MH/$f" || heavy_fail "remove did not restore $f byte for byte"
  else
    [[ ! -e "$MH/$f" ]] || heavy_fail "remove left $f, which apply created"
  fi
done
setup "$MH" remove --yes
heavy_client_must_say "$SETUP_OUT" "Nothing managed" "a second remove still found something"

# ── 8. Two npm proxies and a local one, by reload ────────────────────────────

api() {  # method path
  curl -sS --fail-with-body -X "$1" -H "Authorization: Bearer $ADMIN_TOKEN" "$HEAVY_BASE$2"
}
reload_config() {  # <python edit of the TOML text, reading `t`>
  python3 - "$CONFIG" "$1" <<'PY'
import sys
path, expr = sys.argv[1], sys.argv[2]
t = open(path).read()
t = eval(expr)
open(path + ".next", "w").write(t)
PY
  mv "$CONFIG.next" "$CONFIG"
  for _ in $(seq 1 30); do
    api GET /api/v1/admin/config/pending > "$HEAVY_WORK/pending.json" 2>/dev/null \
      && grep -q '"registries' "$HEAVY_WORK/pending.json" && break
    sleep 1
  done
  api POST /api/v1/admin/config/pending/apply > "$HEAVY_WORK/applied.json" \
    || { cat "$HEAVY_WORK/pending.json" "$HEAVY_WORK/applied.json" >&2; heavy_fail "the reload was not applied"; }
}

heavy_log "8. a second npm proxy and a local npm, by reload"
EXTRA_NPM="
[[registries]]
type = \"npm\"
name = \"$NPM_B\"
mode = \"proxy\"

[registries.rbac]
anonymous = [\"releases:read\", \"releases:list\", \"source:read\"]
admin = [\"*\"]

[[registries]]
type = \"npm\"
name = \"$NPM_LOCAL\"
mode = \"local\"

[registries.rbac]
anonymous = [\"releases:read\", \"releases:list\", \"source:read\"]
admin = [\"*\"]
"
reload_config "t + '''$EXTRA_NPM'''"
curl -fsS "$HEAVY_TAP_BASE/api/v1/registries" | grep -q "\"$NPM_B\"" \
  || heavy_fail "the reloaded registry list does not name $NPM_B"

M8="$HEAVY_WORK/machine-pick"
setup "$M8" status --only npm,maven,pip
expect_code 3 "status with two npm proxies"
heavy_client_must_say "$SETUP_OUT" "ambiguous.*$NPM_B|$NPM_B.*ambiguous|npm: .*$NPM_B" "npm is not ambiguous between the two proxies"
grep -q "$NPM_LOCAL" "$SETUP_OUT" && { cat "$SETUP_OUT" >&2; heavy_fail "a local registry was offered as a mirror candidate"; }
setup "$M8" apply --yes --only npm,maven,pip --pick "npm=$NPM_B" --maven-mirror-of all
expect_code 0 "apply with a pick"
grep -q "^registry=$HEAVY_TAP_BASE/proxy/$NPM_B/$" "$M8/.npmrc" || heavy_fail "the pick was not written"
grep -q "<mirrorOf>\*</mirrorOf>" "$M8/.m2/settings.xml" || heavy_fail "--maven-mirror-of all was not written"
setup "$M8" apply --yes --only npm,maven,pip
expect_code 0 "a plain apply after a pick"
heavy_client_must_say "$SETUP_OUT" "Nothing to write" "a plain apply did not keep the pick and the mirror form"

# ── 9. The pypi registry removed: orphaned ───────────────────────────────────

heavy_log "9. the pypi registry removed, by reload"
reload_config "t[:t.index('# ── pip, uv')] + t[t.index('# ── go:'):]"
setup "$M8" status --only pip
expect_code 3 "status with the pypi registry gone"
heavy_client_must_say "$SETUP_OUT" "orphaned" "pip's block is not reported orphaned"
setup "$M8" apply --yes --only pip
expect_code 3 "apply over an orphaned block"
heavy_client_must_say "$SETUP_OUT" "no longer listed" "apply did not say why pip's block is kept"
grep -q "batlehub setup" "$M8/.config/pip/pip.conf" \
  || heavy_fail "apply took the orphaned pip block out: pip would fall back to pypi.org"
setup "$M8" remove --yes --only pip
expect_code 0 "remove of an orphaned block"
[[ ! -e "$M8/.config/pip/pip.conf" ]] || heavy_fail "remove left the orphaned pip block (and the file apply created)"

heavy_done "CLIENT-SETUP-HEAVY-OK"
