#!/usr/bin/env bash
# Heavy closed-world suite — one registry kind per phase, each proving the same
# sentence: with the client able to reach nothing but this BatleHub, the
# dependencies come from it, the build runs, and the program runs.
#
# BatleHub keeps its egress; the package manager, or whatever tool stands in
# for one, gets none. That asymmetry is the whole design: the instance is the
# only route to the internet, so anything a phase obtains, it obtained here.
#
# `tests/heavy/rustup.sh` is this claim for Rust, and proves one thing more —
# there the compiler comes from the instance too, because rustup *is* the
# package manager. §11 (nvm), §12 (SDKMAN), §16 (the JetBrains Runtime) and
# §19 (helm) make that same stronger claim for their kinds: what crosses the
# proxy is the runtime or the tool, and it is then executed. Where the package
# manager is npm, pip, Maven, Bundler, NuGet, Composer, conda or the Go tool,
# the runtime comes with the runner and what has to survive the closed world is
# the resolve and the build.
#
# Two phases end differently, and say so where they do: an editor extension
# (§13, §14) and an IDE plugin (§15) are not programs, so what is asserted is
# that the artifact the instance served is the one that was asked for, opened —
# and, for the two that go through an editor, listed back by it.
#
# The world is closed as `mise.sh` §4 and `airgap.sh` close it — every client
# process runs with HTTP(S)_PROXY set and only the loopback exempted — with one
# difference: the proxy here is a *running* one that relays the loopback and
# refuses every other host (`closed_proxy.py`), not a port nothing listens on.
# A dead port closes the world only for a client that honours `NO_PROXY`, and
# VS Code's server CLI does not read it, so it sent its request for this
# instance to the dead port and the closed world was closed to the instance
# too. §0 asserts the denial against two real hosts before any phase depends on
# it — a phase that passes with egress open is green for the wrong reason, and
# that is the failure mode this suite exists to avoid. The *server* keeps its
# egress: it is the one process allowed upstream.
#
# One file rather than twenty-two, following `authz.sh`: the machinery (the
# denial, the control probe, the build-and-run assertion, the transcript
# scoping) is identical across kinds and only the client differs, so twenty-two
# copies would drift. Each phase is selected by name and runs against its own
# registry on one instance.
#
#   bash tests/heavy/closed_world.sh go        # one kind
#   bash tests/heavy/closed_world.sh all       # every kind, one server
#
# Two phases run their client inside that client's own distribution image —
# `dnf` in AlmaLinux 9 and `pacman` in Arch — because a distribution package is
# linked against its own libraries and cannot be executed anywhere else. They
# need podman or docker, and nothing else; the runner no longer has to host a
# foreign package manager. See "The three OS package managers" below.
#
# Three phases can be left unmeasured: `HEAVY_CW_SKIP_RPM=1` and
# `HEAVY_CW_SKIP_PACMAN=1` for a machine with no container engine, and
# `HEAVY_CW_SKIP_JB=1` for one that cannot spare the ~1.5 GB IntelliJ. Each
# prints a SKIPPED banner naming the row it did not measure; absent the
# variable, a missing client is a failed run rather than a quiet pass.
#
# Every phase's program prints `CLOSED-WORLD-RAN <value>` and the assertion is
# that line: a build that links a dependency proves the download, and a program
# that prints what the dependency computed proves the code in it ran.
#
# Run via `task test:closed-world-heavy` (all phases) or `task
# test:closed-world-heavy -- go`. Needs network *for the server*: the upstreams
# are the twenty-two public registries in `config.closed-world.toml`.
# Environment knobs: DATABASE_URL (required), HEAVY_PORT (8155), HEAVY_TAP_PORT
# (8156), HEAVY_TF_TAP_PORT (8157, the TLS tap the Terraform phase needs),
# COVERAGE, and one `HEAVY_CW_*` pin per client the suite resolves for itself —
# declared together below, so the version a phase asserts against is in one
# place rather than in twenty-two.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

heavy_init closed_world 8155 8156
heavy_need curl "curl"
heavy_need python3 "python3 (the wire tap)"

# Ordered so the cheap phases fail first: a broken instance is better found by
# `go` in a minute than by `jbplugin` after a 1.5 GB download.
PHASES=(go node python java ruby dotnet php conda terraform mise
        nvm sdkman ovsx helm apt forgejo gitlab jbr vscode jbplugin dnf pacman)
WANTED=("$@")
[[ "${#WANTED[@]}" == 0 || "${WANTED[0]}" == "all" ]] && WANTED=("${PHASES[@]}")
for want in "${WANTED[@]}"; do
  printf '%s\n' "${PHASES[@]}" | grep -qx "$want" \
    || heavy_fail "unknown phase '$want' — one of: ${PHASES[*]}, or all"
done

# Client pins. Only the ones this suite resolves through mise are named here;
# a client the runner already has is used as it is, and its version goes in the
# transcript rather than in a variable.
NODE_DEPS_TYPESCRIPT="${HEAVY_CW_TYPESCRIPT:-5.9.3}"
RUBY_VERSION="${HEAVY_CW_RUBY:-3.3.12}"
# Bundler is pinned separately from the interpreter because §5 may have to
# install one: a distribution ruby is packaged without the binstub.
BUNDLER_VERSION="${HEAVY_CW_BUNDLER:-4.0.17}"
JAVA_VERSION="${HEAVY_CW_JAVA:-temurin-21.0.12+101.0.LTS}"
MAVEN_VERSION="${HEAVY_CW_MAVEN:-3.9.16}"
DOTNET_VERSION="${HEAVY_CW_DOTNET:-10.0}"
TERRAFORM_VERSION="${HEAVY_CW_TERRAFORM:-1.8.5}"
MISE_TOOL="${HEAVY_CW_MISE_TOOL:-github:cli/cli}"
MISE_TOOL_VERSION="${HEAVY_CW_MISE_TOOL_VERSION:-2.60.0}"
PHP_VERSION="${HEAVY_CW_PHP:-8.3.28}"
COMPOSER_VERSION="${HEAVY_CW_COMPOSER:-2.10.2}"
MICROMAMBA_VERSION="${HEAVY_CW_MICROMAMBA:-2.9.0}"

# The second half's pins. A *tool* installer is pinned twice over — the client
# and the thing it installs — because both are versions this suite asserts
# against, and a floating one turns an aged release into a failure that reads
# like a server bug.
NVM_VERSION="${HEAVY_CW_NVM:-0.40.3}"
NODE_DIST_VERSION="${HEAVY_CW_NODE_DIST:-22.11.0}"
SDKMAN_CLI_VERSION="${HEAVY_CW_SDKMAN_CLI:-5.23.0}"
# The java version is *not* pinned: the phase reads it from the instance's own
# `candidates/default/java`, so it asks for whatever SDKMAN would have given a
# developer that day. `HEAVY_CW_SDKMAN_JAVA` overrides.
OVSX_VERSION="${HEAVY_CW_OVSX:-1.1.1}"
OVSX_EXT="${HEAVY_CW_OVSX_EXT:-redhat.vscode-yaml}"
VSCODE_VERSION="${HEAVY_CW_VSCODE:-1.136.2}"
VSCODE_EXT="${HEAVY_CW_VSCODE_EXT:-redhat.vscode-yaml}"
IDEA_VERSION="${HEAVY_CW_IDEA:-2026.1.3}"
JB_PLUGIN="${HEAVY_CW_JB_PLUGIN:-IdeaVIM}"
# What the installed plugin's directory is called on disk, which is not the id.
JB_PLUGIN_DIR_MATCH="${HEAVY_CW_JB_PLUGIN_DIR:-ideavim}"
JBR_PATH="${HEAVY_CW_JBR_PATH:-intellij-jbr/jbr_jcef-21.0.5-linux-x64-b631.30.tar.gz}"
FORGEJO_TOOL="${HEAVY_CW_FORGEJO_TOOL:-forgejo/forgejo}"
FORGEJO_TOOL_VERSION="${HEAVY_CW_FORGEJO_TOOL_VERSION:-16.0.4}"
FORGEJO_TOOL_BIN="${HEAVY_CW_FORGEJO_TOOL_BIN:-forgejo}"
GITLAB_TOOL="${HEAVY_CW_GITLAB_TOOL:-gitlab-org/cli}"
GITLAB_TOOL_VERSION="${HEAVY_CW_GITLAB_TOOL_VERSION:-1.117.0}"
GITLAB_TOOL_BIN="${HEAVY_CW_GITLAB_TOOL_BIN:-glab}"
HELM_VERSION="${HEAVY_CW_HELM:-3.16.4}"
# The three OS package managers: the package, the path its binary lands on once
# the archive is unpacked, and the first thing that binary prints — which is
# what `cw_ran` matches, so it is declared beside the package rather than
# buried in the phase.
APT_SUITE="${HEAVY_CW_APT_SUITE:-noble}"
APT_PACKAGE="${HEAVY_CW_APT_PACKAGE:-hello}"
APT_PACKAGE_BIN="${HEAVY_CW_APT_PACKAGE_BIN:-usr/bin/hello}"
APT_PACKAGE_RAN="${HEAVY_CW_APT_PACKAGE_RAN:-Hello, world!}"
APT_KEYRING="${HEAVY_CW_APT_KEYRING:-/usr/share/keyrings/ubuntu-archive-keyring.gpg}"
# `pv`, not `htop`: only the mirrored repository is enabled, so the package has
# to resolve inside it. EPEL's htop needs `libnl`, `libnl-genl` and `libhwloc`,
# which live in AlmaLinux's own BaseOS/AppStream and are not in the base image —
# `nothing provides libnl-3.so.200()(64bit)`, a dependency solver saying the
# repository set is short, not that the proxy failed. `pv` requires `libc` and
# `rtld` and nothing else, so what the phase measures is the mirror.
DNF_PACKAGE="${HEAVY_CW_DNF_PACKAGE:-pv}"
DNF_PACKAGE_CMD="${HEAVY_CW_DNF_PACKAGE_CMD:-pv}"
DNF_PACKAGE_RAN="${HEAVY_CW_DNF_PACKAGE_RAN:-pv}"
PACMAN_PACKAGE="${HEAVY_CW_PACMAN_PACKAGE:-jq}"
PACMAN_PACKAGE_CMD="${HEAVY_CW_PACMAN_PACKAGE_CMD:-jq}"
PACMAN_PACKAGE_RAN="${HEAVY_CW_PACMAN_PACKAGE_RAN:-jq-}"
# The two containerised clients' images. Neither is on Docker Hub, and that is
# the point: Docker Hub's anonymous pull budget is counted per source IP, and a
# hosted runner's IP is shared with every other job on that machine — the same
# arithmetic that makes the forge suites' anonymous GitHub budget unreliable.
RPM_IMAGE="${HEAVY_CW_RPM_IMAGE:-quay.io/almalinuxorg/almalinux:9}"
PACMAN_IMAGE="${HEAVY_CW_PACMAN_IMAGE:-ghcr.io/archlinux/archlinux:base}"

GO_REG="go-$HEAVY_RUN"
NPM_REG="npm-$HEAVY_RUN"
PYPI_REG="pypi-$HEAVY_RUN"
MAVEN_REG="maven-$HEAVY_RUN"
GEMS_REG="gems-$HEAVY_RUN"
NUGET_REG="nuget-$HEAVY_RUN"
COMPOSER_REG="composer-$HEAVY_RUN"
CONDA_REG="conda-$HEAVY_RUN"
TF_REG="tf-$HEAVY_RUN"
GITHUB_REG="github-$HEAVY_RUN"
NODEDIST_REG="nodedist-$HEAVY_RUN"
SDKMAN_REG="sdkman-$HEAVY_RUN"
OVSX_REG="ovsx-$HEAVY_RUN"
VSCODE_REG="vscode-$HEAVY_RUN"
JBM_REG="jbm-$HEAVY_RUN"
JB_REG="jb-$HEAVY_RUN"
FORGEJO_REG="forgejo-$HEAVY_RUN"
GITLAB_REG="gitlab-$HEAVY_RUN"
HELM_REG="helm-$HEAVY_RUN"
DEB_REG="deb-$HEAVY_RUN"
RPM_REG="rpm-$HEAVY_RUN"
PACMAN_REG="pacman-$HEAVY_RUN"

heavy_forge_auth_config tests/heavy/config.closed-world.toml
heavy_start_server "$HEAVY_CONFIG"
heavy_start_tap

# The client-side egress denial, as in mise.sh §4 and airgap.sh: a proxy on a
# closed port, with the loopback exempted so the tap is reachable. Named once
# because all six variables have to name the *same* closed port — a typo in one
# of them leaves that scheme reaching the real internet, and the phase still
# passes.
# A *running* proxy that relays the loopback and refuses everything else, not
# the closed port the other suites use. `NO_PROXY` is still set and still does
# the work for every client that honours it; this exists for the one that does
# not. VS Code's server CLI reads `HTTP_PROXY` and nothing else — no `no_proxy`
# in that code path at any version — so `--install-extension` sent its gallery
# request for the tap to the closed port and failed with `ECONNREFUSED
# 127.0.0.1:1`, a closed world that was closed to this instance too. See the
# module docs on `closed_proxy.py`; §0 proves the denial on every run.
CLOSED_PROXY_PORT="${HEAVY_CW_DENY_PORT:-8158}"
CLOSED_PROXY="http://127.0.0.1:$CLOSED_PROXY_PORT"
LOOPBACK_DIRECT="127.0.0.1,localhost"
DENY=(env HTTP_PROXY="$CLOSED_PROXY" HTTPS_PROXY="$CLOSED_PROXY"
      http_proxy="$CLOSED_PROXY" https_proxy="$CLOSED_PROXY"
      NO_PROXY="$LOOPBACK_DIRECT" no_proxy="$LOOPBACK_DIRECT")

python3 tests/heavy/closed_proxy.py "$CLOSED_PROXY_PORT" \
  >"$HEAVY_WORK/closed-proxy.err" 2>&1 &
HEAVY_EXTRA_PIDS+=("$!")
for _ in $(seq 1 50); do
  (exec 3<>"/dev/tcp/127.0.0.1/$CLOSED_PROXY_PORT") 2>/dev/null && break
  sleep 0.1
done
(exec 3<>"/dev/tcp/127.0.0.1/$CLOSED_PROXY_PORT") 2>/dev/null \
  || heavy_fail "the closed-world proxy never came up on $CLOSED_PROXY_PORT"

# ── §0. The world is closed ──────────────────────────────────────────────────

# What counts as closed is narrow on purpose. The denial is a proxy that
# answers `403 closed world: …` now, not a port that refuses a connection, and
# `curl` exits 0 on a 403 — a probe that only looked at the exit status would
# read every refusal as a success and fail the run, and a probe that took any
# non-2xx as closed would read a real registry's own 4xx as closed. So the
# refusal has to be *this* proxy's, or no connection at all.
heavy_log "Egress control: a client process reaching two real registries"
for host in "https://registry.npmjs.org/left-pad" "https://proxy.golang.org/rsc.io/quote/@v/list"; do
  status="$("${DENY[@]}" curl -sS --max-time 20 -w '%{http_code}' \
    -o "$HEAVY_WORK/egress.body" "$host" 2>"$HEAVY_WORK/egress.txt")" || status="unreachable"
  case "$status" in
    403)
      grep -q "closed world" "$HEAVY_WORK/egress.body" \
        || heavy_fail "$host answered 403 from something other than the closed-world proxy — the denial is not the one this suite thinks it is"
      ;;
    unreachable) ;;
    *)
      heavy_fail "a client process reached $host ($status) — the world is not closed, and every phase below would pass for the wrong reason"
      ;;
  esac
done
heavy_log "CLOSED-WORLD-EGRESS-OK (the public registries are unreachable from the client side)"

# cw_out <phase> — the file a phase's client output goes to.
cw_out() { echo "$HEAVY_WORK/$1.txt"; }

# cw_step <out> <dir> <cmd...> — run a build step in <dir>, capturing to <out>.
# The egress denial is *not* added here: each phase prepends `"${DENY[@]}"`
# itself, because most also have to pass the client's own registry variables
# and a wrapper that hid one of the two would hide which.
cw_step() {
  local out="$1" dir="$2"
  shift 2
  (cd "$dir" && "$@") >"$out" 2>&1
  return $?
}

# cw_ran <phase> <output-file> <expected> — the program printed what the
# dependency computed. One banner across every ecosystem, so the assertion is
# the same sentence in ten languages.
cw_ran() {
  local phase="$1" out="$2" expected="$3"
  # Fixed-string: `[1,4,9,16]` is what the .NET phase prints, and as a basic
  # regular expression that is a character class matching one digit.
  grep -qF "CLOSED-WORLD-RAN $expected" "$out" || {
    cat "$out" >&2
    heavy_fail "$phase: the program did not print 'CLOSED-WORLD-RAN $expected' — the dependency's code did not run"
  }
}

# cw_ran_from <phase> <out-file> <expected> <client-output> — the banner, for a
# phase whose program is not one this suite wrote.
#
# `deb` ends in a binary from Ubuntu's own archive: it prints what it prints,
# and no amount of arranging makes it print this suite's banner. So the banner
# is *composed from its first line of output* and then asserted exactly as
# every other phase's is — the value still comes from the executed binary,
# which is the thing that has to be true. The other two OS package managers run
# inside a container and can echo the banner there, so only `deb` needs this.
cw_ran_from() {
  local phase="$1" out="$2" expected="$3" ran="$4"
  printf 'CLOSED-WORLD-RAN %s\n' "$(head -1 "$ran" | tr -d '\r')" >"$out"
  cw_ran "$phase" "$out" "$expected"
}

# cw_container_deny — echo the `-e` flags that close the world *inside* a
# container, as one word per line for a caller's `mapfile`/array.
#
# The same six variables as `DENY`, and the same reason they are named once:
# all six have to carry the same closed port, and a typo in one leaves that
# scheme reaching the real internet with the phase still passing. They are
# passed by value rather than inherited, because `docker run` inherits nothing
# and `-e NAME` would forward this shell's (unset) value.
cw_container_deny() {
  printf '%s\n' \
    -e "HTTP_PROXY=$CLOSED_PROXY" -e "HTTPS_PROXY=$CLOSED_PROXY" \
    -e "http_proxy=$CLOSED_PROXY" -e "https_proxy=$CLOSED_PROXY" \
    -e "NO_PROXY=$LOOPBACK_DIRECT" -e "no_proxy=$LOOPBACK_DIRECT"
}

# ── §1. Go ───────────────────────────────────────────────────────────────────
#
# `rsc.io/quote` is three modules deep (quote → sampler → golang.org/x/text),
# so the resolve is a walk and the transcript shows every hop. `GOTOOLCHAIN=local`
# keeps the Go tool from downloading a toolchain of its own, which it would try
# to do off `go.mod` and which no phase here is measuring.

phase_go() {
  heavy_need go "the Go toolchain"
  local dir="$HEAVY_WORK/go" cache="$HEAVY_WORK/go-cache" out
  out="$(cw_out go)"
  mkdir -p "$dir" "$cache"
  cat >"$dir/go.mod" <<'EOF'
module closedworld

go 1.21

require rsc.io/quote v1.5.2
EOF
  cat >"$dir/main.go" <<'EOF'
package main

import (
	"fmt"
	"strings"

	"rsc.io/quote"
)

func main() {
	fmt.Println("CLOSED-WORLD-RAN", strings.TrimSuffix(quote.Hello(), "."))
}
EOF

  heavy_mark go
  heavy_log "go mod tidy + go build + run, with egress denied"
  # `GOSUMDB=off` rather than a proxied sum database: what this phase measures
  # is the module path, and `tests/heavy/go.sh` already owns the sumdb.
  # `-modcacherw` is not a nicety: without it the module cache is written
  # read-only and the suite's own `rm -rf` of its work directory fails, which
  # turns a passing phase into a non-zero exit at cleanup.
  #
  # `LANG`/`LC_ALL` are pinned because the *dependency* reads them: `rsc.io/quote`
  # calls `rsc.io/sampler`, which greets in the caller's locale — "Hello, world"
  # unset, "Bonjour le monde" under `fr_FR.UTF-8`, and "Ahoy, world!" under the
  # `C.UTF-8` that GitHub runners set. The assertion is what the dependency
  # computed, so the locale it computes in belongs beside it rather than in the
  # runner's environment. No locale has to be *installed*: sampler matches the
  # tag with `golang.org/x/text`, not with the system's locale database.
  local goenv=(GOPROXY="$HEAVY_TAP_BASE/proxy/$GO_REG" GOFLAGS="-mod=mod -modcacherw"
               GOTOOLCHAIN=local GOSUMDB=off LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8
               GOMODCACHE="$cache/mod" GOCACHE="$cache/build" GOPATH="$cache/gopath")
  cw_step "$out" "$dir" "${DENY[@]}" "${goenv[@]}" go mod tidy \
    || { cat "$out" >&2; heavy_fail "go: the resolve failed inside the closed world"; }
  cw_step "$out" "$dir" "${DENY[@]}" "${goenv[@]}" go build -o app . \
    || { cat "$out" >&2; heavy_fail "go: the build failed inside the closed world"; }

  heavy_wire_after go "GET /proxy/$GO_REG/rsc.io/quote/@v/v1.5.2.mod -> 200" \
    "the Go tool did not read rsc.io/quote's go.mod through the proxy"
  heavy_wire_after go "GET /proxy/$GO_REG/rsc.io/quote/@v/v1.5.2.zip -> 200" \
    "the module zip did not come through the proxy"
  local modules
  modules="$(heavy_wire_count_after go "GET /proxy/$GO_REG/.*/@v/.*[.]zip -> 200")"
  [[ "$modules" -ge 2 ]] \
    || heavy_fail "go: only $modules module zip(s) came through the proxy — the transitive modules did not, so the resolve was not a walk"

  cw_step "$out" "$dir" "${DENY[@]}" ./app \
    || { cat "$out" >&2; heavy_fail "go: the built binary did not run"; }
  cw_ran go "$out" "Hello, world"
  heavy_log "CLOSED-WORLD-GO-OK ($(head -1 "$out"), $modules modules through the proxy)"
}

# ── §2. Node ─────────────────────────────────────────────────────────────────
#
# A real compile, not only an install: TypeScript comes from the proxied
# registry and compiles the program that the runtime dependency is then called
# from. `npm install` is given the registry and nothing else — no audit, no
# funding lookup, both of which are requests to npmjs.org that the denial would
# turn into a failed install for a reason unrelated to the claim.

phase_node() {
  heavy_need npm "nodejs"
  heavy_need node "nodejs"
  local dir="$HEAVY_WORK/node" out
  out="$(cw_out node)"
  mkdir -p "$dir/src"
  cat >"$dir/package.json" <<EOF
{
  "name": "closed-world",
  "version": "1.0.0",
  "private": true,
  "dependencies": { "left-pad": "1.3.0" },
  "devDependencies": { "typescript": "$NODE_DEPS_TYPESCRIPT" }
}
EOF
  cat >"$dir/tsconfig.json" <<'EOF'
{
  "compilerOptions": {
    "target": "es2020",
    "module": "commonjs",
    "outDir": "dist",
    "strict": true
  },
  "include": ["src"]
}
EOF
  cat >"$dir/src/index.ts" <<'EOF'
// Declared rather than typed from `@types/node`: the phase is about the
// compiler and the dependency reaching this machine, and a types package would
// add a third download and a version to keep in step for nothing.
declare function require(name: string): unknown;


const leftPad = require("left-pad") as (s: string, n: number, c?: string) => string;
const padded: string = leftPad("42", 6, "0");
console.log(`CLOSED-WORLD-RAN ${padded}`);
EOF
  cat >"$dir/.npmrc" <<EOF
registry=$HEAVY_TAP_BASE/proxy/$NPM_REG/
audit=false
fund=false
update-notifier=false
EOF

  heavy_mark node
  heavy_log "npm install + tsc + node, with egress denied"
  cw_step "$out" "$dir" "${DENY[@]}" npm_config_cache="$HEAVY_WORK/npm-cache" \
    npm install --no-audit --no-fund \
    || { cat "$out" >&2; heavy_fail "node: npm install failed inside the closed world"; }
  heavy_wire_after node "GET /proxy/$NPM_REG/left-pad -> 200" \
    "npm did not read left-pad's packument through the proxy"
  # `{name}/{version}/tarball`, the path this server writes into the packument,
  # not npm's own `{name}/-/{name}-{v}.tgz`.
  heavy_wire_after node "GET /proxy/$NPM_REG/typescript/$NODE_DEPS_TYPESCRIPT/tarball -> 200" \
    "the TypeScript tarball did not come through the proxy"
  heavy_wire_after node "GET /proxy/$NPM_REG/left-pad/1.3.0/tarball -> 200" \
    "the runtime dependency's tarball did not come through the proxy"

  cw_step "$out" "$dir" "${DENY[@]}" ./node_modules/.bin/tsc \
    || { cat "$out" >&2; heavy_fail "node: tsc failed to compile inside the closed world"; }
  [[ -f "$dir/dist/index.js" ]] || heavy_fail "node: tsc produced no dist/index.js"
  cw_step "$out" "$dir" "${DENY[@]}" node dist/index.js \
    || { cat "$out" >&2; heavy_fail "node: the compiled program did not run"; }
  cw_ran node "$out" "000042"
  heavy_log "CLOSED-WORLD-NODE-OK (tsc $NODE_DEPS_TYPESCRIPT compiled it, $(head -1 "$out"))"
}

# ── §3. Python ───────────────────────────────────────────────────────────────
#
# Two things, because pip does two: it *builds* — a wheel from a source tree,
# with the build backend fetched into an isolated environment, which is the
# step that reaches the index twice — and it installs a dependency with a
# transitive one of its own. Both through the proxy, both with egress denied.

phase_python() {
  heavy_need python3 "python3 with venv"
  local dir="$HEAVY_WORK/python" venv="$HEAVY_WORK/python-venv" out simple
  out="$(cw_out python)"
  simple="$HEAVY_TAP_BASE/proxy/$PYPI_REG/simple/"
  mkdir -p "$dir/src/closedworld"
  cat >"$dir/pyproject.toml" <<'EOF'
[build-system]
requires = ["setuptools>=68"]
build-backend = "setuptools.build_meta"

[project]
name = "closedworld"
version = "1.0.0"
dependencies = ["python-dateutil"]

[tool.setuptools.packages.find]
where = ["src"]
EOF
  cat >"$dir/src/closedworld/__init__.py" <<'EOF'
from dateutil.parser import parse


def ran() -> str:
    return parse("2026-09-12T00:00:00").strftime("%Y/%m/%d")
EOF
  cat >"$dir/run.py" <<'EOF'
from closedworld import ran

print("CLOSED-WORLD-RAN", ran())
EOF

  heavy_mark python
  heavy_log "python -m venv (no egress needed: ensurepip is local)"
  python3 -m venv "$venv" >"$out" 2>&1 \
    || { cat "$out" >&2; heavy_fail "python: could not create the virtual environment"; }

  heavy_log "pip wheel (the build backend comes through the proxy) + pip install + run"
  cw_step "$out" "$dir" "${DENY[@]}" PIP_INDEX_URL="$simple" \
    PIP_DISABLE_PIP_VERSION_CHECK=1 PIP_NO_CACHE_DIR=1 \
    "$venv/bin/pip" wheel --wheel-dir "$HEAVY_WORK/python-wheels" . \
    || { cat "$out" >&2; heavy_fail "python: the wheel build failed inside the closed world"; }
  heavy_wire_re_after python "GET /proxy/$PYPI_REG/simple/setuptools/? -> 200" \
    "the build backend was not resolved through the proxy — the build was not isolated, or it reached elsewhere"

  cw_step "$out" "$dir" "${DENY[@]}" PIP_INDEX_URL="$simple" \
    PIP_DISABLE_PIP_VERSION_CHECK=1 PIP_NO_CACHE_DIR=1 \
    "$venv/bin/pip" install --no-index --find-links "$HEAVY_WORK/python-wheels" closedworld \
    || { cat "$out" >&2; heavy_fail "python: installing the built wheel failed"; }
  heavy_wire_re_after python "GET /proxy/$PYPI_REG/simple/python-dateutil/? -> 200" \
    "the dependency's simple page did not come through the proxy"

  cw_step "$out" "$dir" "${DENY[@]}" "$venv/bin/python" run.py \
    || { cat "$out" >&2; heavy_fail "python: the program did not run"; }
  cw_ran python "$out" "2026/09/12"
  heavy_log "CLOSED-WORLD-PYTHON-OK (wheel built and installed, $(head -1 "$out"))"
}

# ── §4. Java ─────────────────────────────────────────────────────────────────
#
# The strongest form of the claim in this file: Maven fetches its *plugins* from
# the same mirror as the dependencies, so a `mvn package` inside the closed
# world is a hundred artifacts through the proxy and stops on the first one it
# cannot get. The local repository lives in the run's directory, so nothing is
# answered out of the runner's `~/.m2`.

phase_java() {
  heavy_runner_for mvn "maven@$MAVEN_VERSION" "java@$JAVA_VERSION"
  local mvn=("${HEAVY_RUNNER[@]}" mvn)
  local java=("${HEAVY_RUNNER[@]}" java)
  local dir="$HEAVY_WORK/java" repo="$HEAVY_WORK/m2" out
  out="$(cw_out java)"
  mkdir -p "$dir/src/main/java/cw" "$repo"
  cat >"$HEAVY_WORK/settings.xml" <<EOF
<settings>
  <mirrors>
    <mirror>
      <id>closed-world</id>
      <mirrorOf>*</mirrorOf>
      <url>$HEAVY_TAP_BASE/proxy/$MAVEN_REG/maven2</url>
    </mirror>
  </mirrors>
</settings>
EOF
  cat >"$dir/pom.xml" <<'EOF'
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>sh.batlehub</groupId>
  <artifactId>closed-world</artifactId>
  <version>1.0.0</version>
  <properties>
    <maven.compiler.source>17</maven.compiler.source>
    <maven.compiler.target>17</maven.compiler.target>
    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>
  </properties>
  <dependencies>
    <dependency>
      <groupId>org.apache.commons</groupId>
      <artifactId>commons-lang3</artifactId>
      <version>3.19.0</version>
    </dependency>
  </dependencies>
</project>
EOF
  cat >"$dir/src/main/java/cw/App.java" <<'EOF'
package cw;

import org.apache.commons.lang3.StringUtils;

public class App {
    public static void main(String[] args) {
        System.out.println("CLOSED-WORLD-RAN " + StringUtils.reverse("dlrow-desolc"));
    }
}
EOF

  heavy_mark java
  heavy_log "mvn package + dependency:copy-dependencies + java, with egress denied"
  cw_step "$out" "$dir" "${DENY[@]}" \
    "${mvn[@]}" -B -s "$HEAVY_WORK/settings.xml" -Dmaven.repo.local="$repo" package \
    || { cat "$out" >&2; heavy_fail "java: mvn package failed inside the closed world"; }
  cw_step "$out" "$dir" "${DENY[@]}" \
    "${mvn[@]}" -B -s "$HEAVY_WORK/settings.xml" -Dmaven.repo.local="$repo" \
    dependency:copy-dependencies -DoutputDirectory=target/deps \
    || { cat "$out" >&2; heavy_fail "java: copying the dependencies failed inside the closed world"; }

  heavy_wire_after java "GET /proxy/$MAVEN_REG/maven2/org/apache/commons/commons-lang3/3.19.0/commons-lang3-3.19.0.jar -> 200" \
    "the dependency jar did not come through the proxy"
  local artifacts
  artifacts="$(heavy_wire_count_after java "GET /proxy/$MAVEN_REG/maven2/.*[.]jar -> 200")"
  [[ "$artifacts" -ge 10 ]] \
    || heavy_fail "java: only $artifacts jar(s) came through the proxy — Maven's own plugins did not, so the build was answered from a warm repository"

  cw_step "$out" "$dir" "${DENY[@]}" \
    "${java[@]}" -cp "target/classes:target/deps/*" cw.App \
    || { cat "$out" >&2; heavy_fail "java: the compiled class did not run"; }
  cw_ran java "$out" "closed-world"
  heavy_log "CLOSED-WORLD-JAVA-OK ($artifacts jars through the proxy, $(head -1 "$out"))"
}

# ── §5. Ruby ─────────────────────────────────────────────────────────────────
#
# `tzinfo` pulls `concurrent-ruby`, so the compact index is walked rather than
# read once. `BUNDLE_PATH` is inside the run, so a gem already in the runner's
# store cannot answer for one that should have come through the proxy.

# cw_bundler — set CW_BUNDLE to the command that runs Bundler under the ruby
# `heavy_runner_for` resolved. A global array for the same reason HEAVY_RUNNER
# is one: `heavy_fail` inside a `$(…)` ends the subshell only, and the phase
# would carry on with an empty command.
#
# The probe is `ruby`, and the client this phase actually runs is `bundle`.
# They are not the same tool: a distribution ruby — which is what a CI runner
# resolves to — is packaged without the binstub, so the probe passes and the
# first step dies as `env: 'bundle': No such file or directory`, naming a
# missing client as if the closed world had broken the build. When the runner's
# ruby brings its own bundler (a mise toolchain does), that one is used; when it
# does not, one is installed here, the way airgap.sh does it and for the same
# reasons:
#
#   - into a work-local GEM_HOME, because a distribution ruby keeps its gems in
#     a root-owned directory (/var/lib/gems/<abi> on Ubuntu) where `gem install`
#     is a `Gem::FilePermissionError`, and because this run has no business
#     writing into a developer's own prefix;
#   - run *by* the interpreter rather than executed, because RubyGems writes its
#     binstubs as an sh/ruby polyglot whose sh half is `exec "${0%/*}/ruby"` —
#     true in a ruby's own GEM_HOME, false in a private prefix, where executing
#     it directly is `exec: …/gems/bin/ruby: not found` before bundler starts.
#
# That install is the only thing in this phase allowed the open internet, and it
# happens before `heavy_mark`: it provisions the client, exactly as mise
# provisions the interpreter, and nothing it fetches is what the phase asserts.
cw_bundler() {
  CW_BUNDLE=()
  if "${HEAVY_RUNNER[@]}" bundle --version >/dev/null 2>&1; then
    CW_BUNDLE=("${HEAVY_RUNNER[@]}" bundle)
    return 0
  fi
  local rubybin rubydir
  rubybin="$("${HEAVY_RUNNER[@]}" bash -c 'command -v ruby')" \
    || heavy_fail "ruby: the interpreter the runner resolved has no path"
  rubydir="$(dirname "$rubybin")"
  [[ -x "$rubydir/gem" ]] \
    || heavy_fail "ruby: no bundler beside $rubybin and no \`gem\` to install one with"
  export GEM_HOME="$HEAVY_WORK/gems"
  export GEM_PATH="$GEM_HOME"
  mkdir -p "$GEM_HOME"
  if ! "$rubydir/gem" list -i bundler -v "$BUNDLER_VERSION" >/dev/null 2>&1; then
    heavy_log "no bundler beside $rubybin — installing bundler $BUNDLER_VERSION into $GEM_HOME"
    "$rubydir/gem" install bundler -v "$BUNDLER_VERSION" --no-document >/dev/null \
      || heavy_fail "ruby: could not install bundler $BUNDLER_VERSION into $GEM_HOME"
  fi
  CW_BUNDLE=("$rubybin" "$GEM_HOME/bin/bundle" "_${BUNDLER_VERSION}_")
}

phase_ruby() {
  heavy_runner_for ruby "ruby@$RUBY_VERSION"
  cw_bundler
  local bundle=("${CW_BUNDLE[@]}")
  local dir="$HEAVY_WORK/ruby" out
  out="$(cw_out ruby)"
  mkdir -p "$dir"
  cat >"$dir/Gemfile" <<EOF
source "$HEAVY_TAP_BASE/proxy/$GEMS_REG"

gem "tzinfo", "2.0.6"
EOF
  cat >"$dir/app.rb" <<'EOF'
require "tzinfo"

zone = TZInfo::Timezone.get("Europe/Paris")
puts "CLOSED-WORLD-RAN #{zone.identifier}"
EOF

  heavy_mark ruby
  heavy_log "bundle install + bundle exec ruby, with egress denied"
  cw_step "$out" "$dir" "${DENY[@]}" BUNDLE_PATH="$HEAVY_WORK/bundle" \
    BUNDLE_GEMFILE="$dir/Gemfile" \
    "${bundle[@]}" install \
    || { cat "$out" >&2; heavy_fail "ruby: bundle install failed inside the closed world"; }
  heavy_wire_re_after ruby "GET /proxy/$GEMS_REG/info/tzinfo -> 200" \
    "bundler did not read tzinfo's compact-index entry through the proxy"
  local gems
  gems="$(heavy_wire_count_after ruby "GET /proxy/$GEMS_REG/gems/.*[.]gem -> 200")"
  [[ "$gems" -ge 2 ]] \
    || heavy_fail "ruby: only $gems gem(s) came through the proxy — the transitive gem did not"

  cw_step "$out" "$dir" "${DENY[@]}" BUNDLE_PATH="$HEAVY_WORK/bundle" \
    BUNDLE_GEMFILE="$dir/Gemfile" \
    "${bundle[@]}" exec ruby app.rb \
    || { cat "$out" >&2; heavy_fail "ruby: the program did not run"; }
  cw_ran ruby "$out" "Europe/Paris"
  heavy_log "CLOSED-WORLD-RUBY-OK ($gems gems through the proxy, $(head -1 "$out"))"
}

# ── §6. .NET ─────────────────────────────────────────────────────────────────
#
# The target framework is read from the SDK the runner has rather than pinned:
# a `net8.0` project on a 10 SDK resolves a targeting pack *through NuGet*, so a
# pin that did not match would turn this phase into a measurement of that
# instead. `<clear/>` in NuGet.config is what removes nuget.org — without it
# the proxy is merely first in a list that still contains the internet.

phase_dotnet() {
  heavy_runner_for dotnet "dotnet@$DOTNET_VERSION"
  local dotnet=("${HEAVY_RUNNER[@]}" dotnet)
  local dir="$HEAVY_WORK/dotnet" out tfm sdk
  out="$(cw_out dotnet)"
  mkdir -p "$dir"
  sdk="$("${dotnet[@]}" --version 2>/dev/null | head -1)"
  tfm="net${sdk%%.*}.0"
  cat >"$dir/NuGet.config" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <clear />
    <!-- `allowInsecureConnections` is not optional: NuGet refuses a
         plain-HTTP source outright (RFC 0009 §12.4), before it reaches the
         closed world this phase is about. A real deployment is HTTPS. -->
    <add key="closed-world" value="$HEAVY_TAP_BASE/proxy/$NUGET_REG/nuget/v3/index.json" protocolVersion="3" allowInsecureConnections="true" />
  </packageSources>
</configuration>
EOF
  cat >"$dir/app.csproj" <<EOF
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
    <TargetFramework>$tfm</TargetFramework>
    <Nullable>disable</Nullable>
    <ImplicitUsings>disable</ImplicitUsings>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.3" />
  </ItemGroup>
</Project>
EOF
  cat >"$dir/Program.cs" <<'EOF'
using System;
using Newtonsoft.Json;

class Program
{
    static void Main()
    {
        var squares = new[] { 1, 4, 9, 16 };
        Console.WriteLine("CLOSED-WORLD-RAN " + JsonConvert.SerializeObject(squares));
    }
}
EOF

  heavy_mark dotnet
  heavy_log "dotnet build + dotnet run (SDK $sdk, $tfm), with egress denied"
  cw_step "$out" "$dir" "${DENY[@]}" \
    DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1 \
    NUGET_PACKAGES="$HEAVY_WORK/nuget-packages" \
    NUGET_HTTP_CACHE_PATH="$HEAVY_WORK/nuget-http-cache" \
    "${dotnet[@]}" build -c Release \
    || { cat "$out" >&2; heavy_fail "dotnet: the build failed inside the closed world"; }
  heavy_wire_re_after dotnet "GET /proxy/$NUGET_REG/nuget/v3/index.json -> 200" \
    "NuGet did not read the service index through the proxy"
  heavy_wire_re_after dotnet "GET /proxy/$NUGET_REG/.*[Nn]ewtonsoft[^ ]*[.]nupkg -> 200" \
    "the package did not come through the proxy"

  cw_step "$out" "$dir" "${DENY[@]}" \
    DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 \
    NUGET_PACKAGES="$HEAVY_WORK/nuget-packages" \
    "${dotnet[@]}" run -c Release --no-build \
    || { cat "$out" >&2; heavy_fail "dotnet: the built program did not run"; }
  cw_ran dotnet "$out" "[1,4,9,16]"
  heavy_log "CLOSED-WORLD-DOTNET-OK (SDK $sdk, $(grep CLOSED-WORLD-RAN "$out" | head -1))"
}

# ── §7. PHP ──────────────────────────────────────────────────────────────────
#
# `packagist.org` is removed from the repository list, not merely outranked:
# Composer's default repository is implicit, and leaving it in place would let
# the resolve fall back to a host the denial then refuses, which fails the
# install for the wrong reason. `psr/log` comes with `monolog/monolog`, so the
# resolve walks.

phase_php() {
  local dir="$HEAVY_WORK/php" out php composer=()
  out="$(cw_out php)"
  if php --version >/dev/null 2>&1; then
    php="$(command -v php)"
  else
    local php_dir
    php_dir="$(heavy_cached_dir "static-php-$PHP_VERSION" \
      "https://dl.static-php.dev/static-php-cli/common/php-$PHP_VERSION-cli-linux-x86_64.tar.gz" tar.gz)"
    php="$php_dir/php"
    [[ -x "$php" ]] || heavy_fail "php: the static archive contained no php binary at $php"
  fi
  if command -v composer >/dev/null 2>&1 && composer --version >/dev/null 2>&1; then
    composer=(composer)
  else
    local phar="$HEAVY_CACHE/composer-$COMPOSER_VERSION.phar"
    if [[ ! -f "$phar" ]]; then
      heavy_log "Downloading composer $COMPOSER_VERSION"
      curl -fsSL --proto '=https' --proto-redir '=https' \
        -o "$phar" "https://getcomposer.org/download/$COMPOSER_VERSION/composer.phar" \
        || heavy_fail "php: could not download composer.phar $COMPOSER_VERSION"
    fi
    composer=("$php" "$phar")
  fi

  mkdir -p "$dir"
  cat >"$dir/composer.json" <<EOF
{
  "name": "batlehub/closed-world",
  "repositories": [
    { "type": "composer", "url": "$HEAVY_TAP_BASE/proxy/$COMPOSER_REG" },
    { "packagist.org": false }
  ],
  "require": { "monolog/monolog": "3.9.0" },
  "config": { "secure-http": false }
}
EOF
  cat >"$dir/app.php" <<'EOF'
<?php
require __DIR__ . '/vendor/autoload.php';

$logger = new Monolog\Logger('closed-world');
echo "CLOSED-WORLD-RAN " . $logger->getName() . PHP_EOL;
EOF

  heavy_mark php
  heavy_log "composer install + php, with egress denied"
  cw_step "$out" "$dir" "${DENY[@]}" \
    COMPOSER_HOME="$HEAVY_WORK/composer-home" \
    COMPOSER_CACHE_DIR="$HEAVY_WORK/composer-cache" \
    COMPOSER_NO_INTERACTION=1 \
    "${composer[@]}" install --no-progress \
    || { cat "$out" >&2; heavy_fail "php: composer install failed inside the closed world"; }
  heavy_wire_after php "GET /proxy/$COMPOSER_REG/packages.json -> 200" \
    "composer did not read packages.json through the proxy"
  local dists
  # `dist/{vendor}/{package}/{version}` — the path this server writes into the
  # p2 document, with no extension on the end, so the count anchors on the
  # prefix rather than on a file suffix that is not there.
  dists="$(heavy_wire_count_after php "GET /proxy/$COMPOSER_REG/dist/[^ ]* -> 200")"
  [[ "$dists" -ge 2 ]] \
    || heavy_fail "php: only $dists package archive(s) came through the proxy — the transitive package did not"

  cw_step "$out" "$dir" "${DENY[@]}" "$php" app.php \
    || { cat "$out" >&2; heavy_fail "php: the program did not run"; }
  cw_ran php "$out" "closed-world"
  heavy_log "CLOSED-WORLD-PHP-OK ($dists archives through the proxy, $(grep CLOSED-WORLD-RAN "$out" | head -1))"
}

# ── §8. conda ────────────────────────────────────────────────────────────────
#
# The one phase where the *interpreter* also comes from the instance: a conda
# environment is built out of the channel, so `python` itself is one of the
# packages the proxy serves. `--override-channels` removes the defaults, which
# are otherwise consulted and are not this instance.
#
# **This is the expensive phase, and the transcript says why.** micromamba asks
# for sharded repodata first (`repodata_shards.msgpack.zst`), which this
# instance does not serve and answers `404`; it then falls back to the whole
# channel index, and takes `linux-64/repodata.json` *uncompressed* — 444 MB —
# before fetching the 72 MB `.zst` beside it in the same run. Roughly half a
# gigabyte crosses the tap for an index used once. `repodata_use_zst` is not
# the lever: it is already micromamba's default (`micromamba config list
# --all`), and setting it changes nothing. Serving sharded repodata would turn
# this into a few hundred kilobytes; until then the cost is what it is, and
# recording it here is better than a knob that looks like a fix.

phase_conda() {
  local out mm dir="$HEAVY_WORK/conda"
  out="$(cw_out conda)"
  local mm_dir
  mm_dir="$(heavy_cached_dir "micromamba-$MICROMAMBA_VERSION" \
    "https://micro.mamba.pm/api/micromamba/linux-64/$MICROMAMBA_VERSION" tar.bz2)"
  mm="$mm_dir/bin/micromamba"
  [[ -x "$mm" ]] || heavy_fail "conda: micromamba was not at $mm"
  mkdir -p "$dir"
  cat >"$dir/run.py" <<'EOF'
import six

print("CLOSED-WORLD-RAN", "six" if six.PY3 else "six2")
EOF

  heavy_mark conda
  heavy_log "micromamba create (python and one package from the channel), with egress denied"
  cw_step "$out" "$dir" "${DENY[@]}" \
    MAMBA_ROOT_PREFIX="$HEAVY_WORK/mamba" CONDA_PKGS_DIRS="$HEAVY_WORK/mamba-pkgs" \
    "$mm" create --yes --quiet --prefix "$HEAVY_WORK/conda-env" \
    --override-channels --channel "$HEAVY_TAP_BASE/proxy/$CONDA_REG" \
    python=3.12 six \
    || { cat "$out" >&2; heavy_fail "conda: the environment could not be built inside the closed world"; }
  heavy_wire_re_after conda "GET /proxy/$CONDA_REG/linux-64/repodata.json -> 200" \
    "micromamba did not read the channel's repodata through the proxy"
  local pkgs
  pkgs="$(heavy_wire_count_after conda "GET /proxy/$CONDA_REG/.*(conda|tar[.]bz2) -> 200")"
  [[ "$pkgs" -ge 2 ]] \
    || heavy_fail "conda: only $pkgs package(s) came through the proxy — the environment was not built from this channel"

  cw_step "$out" "$dir" "${DENY[@]}" "$HEAVY_WORK/conda-env/bin/python" run.py \
    || { cat "$out" >&2; heavy_fail "conda: the program did not run under the environment's own python"; }
  cw_ran conda "$out" "six"
  heavy_log "CLOSED-WORLD-CONDA-OK ($pkgs packages through the proxy, interpreter included)"
}

# ── §9. Terraform ────────────────────────────────────────────────────────────
#
# Terraform refuses a plain-`http:` registry outright (RFC 0009 §12.3), so this
# phase runs against a *second* tap that terminates TLS, on its own port, and
# the source address names `localhost` because that is the one name that
# resolves, is in the certificate, and is bound to the registry in the config.
# `apply` is the run: the provider this instance served is executed.

phase_terraform() {
  heavy_runner_for terraform "terraform@$TERRAFORM_VERSION"
  local tf=("${HEAVY_RUNNER[@]}" terraform)
  local dir="$HEAVY_WORK/terraform" out cert tls_port
  out="$(cw_out terraform)"
  tls_port="${HEAVY_TF_TAP_PORT:-8157}"
  export HEAVY_TAP_HOST=localhost
  cert="$(heavy_self_signed localhost)"
  python3 tests/heavy/http_tap.py "$HEAVY_LOG" "$tls_port" "$HEAVY_PORT" \
    "$cert" "$HEAVY_WORK/tls-key.pem" >"$HEAVY_WORK/tf-tap.err" 2>&1 &
  HEAVY_EXTRA_PIDS+=("$!")
  local probe=(curl -s -o /dev/null --cacert "$cert")
  local i
  for i in $(seq 1 30); do
    "${probe[@]}" "https://localhost:$tls_port/healthz" && break
    sleep 1
  done
  "${probe[@]}" "https://localhost:$tls_port/healthz" \
    || { cat "$HEAVY_WORK/tf-tap.err" >&2; heavy_fail "terraform: the TLS tap never came up on $tls_port"; }

  mkdir -p "$dir"
  cat >"$dir/main.tf" <<EOF
terraform {
  required_providers {
    null = {
      source  = "localhost:$tls_port/hashicorp/null"
      version = "3.2.2"
    }
  }
}

resource "null_resource" "cw" {
  triggers = { value = "terraform" }
}

output "ran" {
  value = "CLOSED-WORLD-RAN \${null_resource.cw.triggers.value}"
}
EOF
  cat >"$HEAVY_WORK/terraformrc" <<EOF
disable_checkpoint = true
EOF

  heavy_mark terraform
  heavy_log "terraform init + apply (the provider comes from the instance), with egress denied"
  cw_step "$out" "$dir" "${DENY[@]}" \
    SSL_CERT_FILE="$cert" TF_IN_AUTOMATION=1 CHECKPOINT_DISABLE=1 \
    TF_CLI_CONFIG_FILE="$HEAVY_WORK/terraformrc" \
    "${tf[@]}" init -no-color \
    || { cat "$out" >&2; heavy_fail "terraform: init failed inside the closed world"; }
  grep -q "successfully initialized" "$out" \
    || { cat "$out" >&2; heavy_fail "terraform: init reported no success line"; }
  heavy_wire_re_after terraform "GET /v1/providers/hashicorp/null/3.2.2/download/linux/amd64 -> 200" \
    "the provider's download document did not come through the proxy"

  cw_step "$out" "$dir" "${DENY[@]}" \
    SSL_CERT_FILE="$cert" TF_IN_AUTOMATION=1 CHECKPOINT_DISABLE=1 \
    TF_CLI_CONFIG_FILE="$HEAVY_WORK/terraformrc" \
    "${tf[@]}" apply -auto-approve -no-color \
    || { cat "$out" >&2; heavy_fail "terraform: apply failed — the provider this instance served did not run"; }
  cw_ran terraform "$out" "terraform"
  heavy_log "CLOSED-WORLD-TERRAFORM-OK (provider fetched and executed)"
}

# ── §10. mise ────────────────────────────────────────────────────────────────
#
# mise is not a package manager for one language: it installs *tools*, from a
# forge, and the thing it produces is a binary to run. The url_replacements are
# the same three rules `mise.sh` uses, which is how a client with no proxy
# setting of its own is pointed at one.
#
# Two installs, because the first one measures something the open-egress suite
# cannot see. With its defaults, mise downloads the asset through the proxy and
# then verifies GitHub artifact attestations — and *that* request is made to
# `api.github.com` directly, not through the rewrite, so it is the one thing in
# this file a closed world stops. The phase records the refusal and its
# wording, turns the setting off, and completes the install; an operator
# running mise behind a proxy has to make the same choice.

phase_mise() {
  heavy_runner_for mise "mise@latest"
  local mise=("${HEAVY_RUNNER[@]}" mise)
  local out proxy="$HEAVY_TAP_BASE/proxy/$GITHUB_REG"
  out="$(cw_out mise)"
  local data="$HEAVY_WORK/mise/data" cache="$HEAVY_WORK/mise/cache"
  local conf="$HEAVY_WORK/mise/config" state="$HEAVY_WORK/mise/state"
  mkdir -p "$data" "$cache" "$conf" "$state"

  # mise_config <true|false> — the settings file, with attestation
  # verification on or off and the three rewrite rules either way.
  mise_config() {
    cat > "$conf/config.toml" <<EOF
[settings]
github_attestations = $1

[settings.url_replacements]
"regex:^https://api\\\\.github\\\\.com/repos/(.+)" = "$proxy/\$1"
"regex:^https://github\\\\.com/([^/]+)/([^/]+)/releases/download/([^/]+)/(.+)" = "$proxy/\$1/\$2/releases/download/\$3/\$4"
"regex:^https://github\\\\.com/([^/]+)/([^/]+)/archive/(?:refs/tags/)?(.+?)\\\\.tar\\\\.gz" = "$proxy/\$1/\$2/tarball/\$3"
EOF
  }

  local miseenv=(MISE_DATA_DIR="$data" MISE_CACHE_DIR="$cache"
                 MISE_CONFIG_DIR="$conf" MISE_STATE_DIR="$state" MISE_YES=1)

  heavy_mark mise
  heavy_log "mise install $MISE_TOOL@$MISE_TOOL_VERSION with its defaults, egress denied"
  mise_config true
  if cw_step "$out" "$HEAVY_WORK" "${DENY[@]}" "${miseenv[@]}" \
    "${mise[@]}" install "$MISE_TOOL@$MISE_TOOL_VERSION"; then
    heavy_log "mise installed with attestation verification on — the check now goes through the proxy, and the arm below is stale"
  else
    grep -qi "attestation" "$out" || {
      cat "$out" >&2
      heavy_fail "mise: the default install failed for a reason other than attestation verification"
    }
    heavy_log "Attestations: mise said:"
    heavy_client_said "$out" 'attestation|error' 3
    # The asset itself *did* come through the proxy: what the closed world
    # stops is the verification call after it, and that call is not on the
    # transcript because it was never addressed here.
    heavy_wire_re_after mise "GET /proxy/$GITHUB_REG/cli/cli/releases/download/[^ ]* -> 200" \
      "the release asset did not come through the proxy, so the failure is not about attestations"
    heavy_wire_not "/attestations/" \
      "an attestation request reached this instance — the rewrite covered it after all, and this arm should become the success path"
  fi

  heavy_log "mise install with github_attestations = false"
  mise_config false
  cw_step "$out" "$HEAVY_WORK" "${DENY[@]}" "${miseenv[@]}" \
    "${mise[@]}" install "$MISE_TOOL@$MISE_TOOL_VERSION" \
    || { cat "$out" >&2; heavy_fail "mise: the install failed inside the closed world with attestation verification off"; }
  heavy_wire_re_after mise "GET /proxy/$GITHUB_REG/cli/cli/releases/[^ ]* -> 200" \
    "mise did not resolve the release through the proxy"

  cw_step "$out" "$HEAVY_WORK" "${DENY[@]}" "${miseenv[@]}" \
    "${mise[@]}" exec "$MISE_TOOL@$MISE_TOOL_VERSION" -- gh --version \
    || { cat "$out" >&2; heavy_fail "mise: the installed tool did not run" ; }
  grep -q "gh version $MISE_TOOL_VERSION" "$out" || {
    cat "$out" >&2
    heavy_fail "mise: the tool that ran is not the version this instance served"
  }
  heavy_log "CLOSED-WORLD-MISE-OK ($(grep -m1 'gh version' "$out"), attestations off)"
}

# ── §11. nvm ─────────────────────────────────────────────────────────────────
#
# `nodedist`, and the strongest shape the claim takes outside Rust: what comes
# through the proxy is the *runtime*, so the program below is executed by a
# Node this instance served rather than by the runner's. `tests/heavy/nvm.sh`
# owns the enforcement half (a blocked release is absent from `index.tab` and
# nvm stops on its own not-found); what is added here is that the install
# completes with nothing else reachable.
#
# nvm is a shell function, not a binary, so it is sourced into a fresh bash
# with `$NVM_DIR` inside the run — never the developer's own.

phase_nvm() {
  local out dir="$HEAVY_WORK/nvm" nvm_src nvm_sh mirror node_bin
  out="$(cw_out nvm)"
  mirror="$HEAVY_TAP_BASE/proxy/$NODEDIST_REG/nodedist"
  nvm_src="$(heavy_cached_dir "nvm-$NVM_VERSION" \
    "https://github.com/nvm-sh/nvm/archive/refs/tags/v$NVM_VERSION.tar.gz")"
  nvm_sh="$nvm_src/nvm-$NVM_VERSION/nvm.sh"
  [[ -f "$nvm_sh" ]] || heavy_fail "nvm: nvm.sh not found under $nvm_src"
  mkdir -p "$dir/home"
  cat >"$dir/app.js" <<'EOF'
// `process.version` rather than a computed value: the claim of this phase is
// *which* Node ran, and that is the one thing only the installed runtime knows.
console.log("CLOSED-WORLD-RAN", process.version);
EOF

  heavy_mark nvm
  heavy_log "nvm install $NODE_DIST_VERSION — the runtime itself through the proxy, with egress denied"
  "${DENY[@]}" NVM_DIR="$dir/home" NVM_NODEJS_ORG_MIRROR="$mirror" \
    bash -c '
      set -o pipefail
      export NVM_DIR NVM_NODEJS_ORG_MIRROR
      # shellcheck disable=SC1090
      source "$1"
      [[ "$(type -t nvm)" == "function" ]] || { echo "nvm.sh did not define nvm" >&2; exit 97; }
      nvm install "$2"
    ' _ "$nvm_sh" "$NODE_DIST_VERSION" >"$out" 2>&1 \
    || { cat "$out" >&2; heavy_fail "nvm: the install failed inside the closed world"; }

  heavy_wire_after nvm "GET /proxy/$NODEDIST_REG/nodedist/index.tab -> 200" \
    "nvm did not read the release index through the proxy"
  # `.tar.xz` when the runner has xz on PATH (nvm_supports_xz), `.tar.gz`
  # otherwise — the same either-or `tests/heavy/nvm.sh` asserts, and the reason
  # this is a regex rather than two fixed strings.
  heavy_wire_re_after nvm \
    "GET /proxy/$NODEDIST_REG/nodedist/v$NODE_DIST_VERSION/node-v$NODE_DIST_VERSION-linux-x64[.]tar[.](xz|gz) -> 200" \
    "the Node tarball did not come through the proxy"
  heavy_wire_after nvm "GET /proxy/$NODEDIST_REG/nodedist/v$NODE_DIST_VERSION/SHASUMS256.txt -> 200" \
    "nvm did not read the checksum file through the proxy — the install was not verified"
  grep -qi "checksums matched" "$out" || {
    cat "$out" >&2
    heavy_fail "nvm: did not report a matching checksum — SHASUMS256.txt did not come through byte-exact (RFC 0010 §7)"
  }

  node_bin="$dir/home/versions/node/v$NODE_DIST_VERSION/bin/node"
  [[ -x "$node_bin" ]] || heavy_fail "nvm: no node at $node_bin after the install"
  cw_step "$out" "$dir" "${DENY[@]}" "$node_bin" app.js \
    || { cat "$out" >&2; heavy_fail "nvm: the installed runtime did not run"; }
  cw_ran nvm "$out" "v$NODE_DIST_VERSION"
  heavy_log "CLOSED-WORLD-NVM-OK (the runtime came from the instance: $(grep CLOSED-WORLD-RAN "$out" | head -1))"
}

# ── §12. SDKMAN ──────────────────────────────────────────────────────────────
#
# Same shape as nvm, one layer further: a JDK, fetched through the *broker*
# route, which is the part of this protocol where the server follows a 302 to
# whichever CDN the vendor named — so the client never learns the CDN's name
# and a closed world cannot stop it.
#
# The version installed is not pinned: it is read from the instance's own
# `candidates/default/java`, so this phase asks for whatever SDKMAN would have
# given a developer that day rather than a string that ages into a 404.
# `sdk` is a shell function; `$SDKMAN_DIR` is assembled by hand exactly as
# `tests/heavy/sdkman.sh` does, because the installer is a `curl | bash` this
# suite will not run.

phase_sdkman() {
  local out dir="$HEAVY_WORK/sdkman/home" api broker cli_root cli_src probe java_bin
  out="$(cw_out sdkman)"
  api="$HEAVY_TAP_BASE/proxy/$SDKMAN_REG/sdkman"
  broker="$api/broker"
  cli_src="$(heavy_cached_dir "sdkman-cli-$SDKMAN_CLI_VERSION" \
    "https://github.com/sdkman/sdkman-cli/releases/download/$SDKMAN_CLI_VERSION/sdkman-cli-$SDKMAN_CLI_VERSION.zip" zip)"
  cli_root="$cli_src/sdkman-$SDKMAN_CLI_VERSION"
  [[ -f "$cli_root/bin/sdkman-init.sh" ]] || heavy_fail "sdkman: no sdkman-init.sh under $cli_src"

  heavy_mark sdkman
  # Every curl below reaches 127.0.0.1, which `NO_PROXY` exempts — so the
  # bootstrap runs under the same denial as the install and there is no window
  # in this phase where the client could have reached anything else.
  mkdir -p "$dir"/{bin,src,contrib,var,tmp,etc,ext,candidates}
  cp -R "$cli_root/bin/." "$dir/bin/"
  cp -R "$cli_root/src/." "$dir/src/"
  cp -R "$cli_root/contrib/." "$dir/contrib/"
  echo "linuxx64" > "$dir/var/platform"
  touch "$dir/var/delay_upgrade"
  "${DENY[@]}" curl -fsS "$api/candidates/all" > "$dir/var/candidates" \
    || heavy_fail "sdkman: could not read candidates/all through the proxy"
  [[ -s "$dir/var/candidates" ]] || heavy_fail "sdkman: candidates/all answered an empty body"
  cat > "$dir/etc/config" <<'EOF'
sdkman_auto_answer=true
sdkman_auto_selfupdate=false
sdkman_selfupdate_feature=false
sdkman_insecure_ssl=false
sdkman_curl_connect_timeout=7
sdkman_curl_max_time=120
sdkman_beta_channel=false
sdkman_debug_mode=false
sdkman_colour_enable=false
sdkman_auto_env=false
sdkman_auto_complete=false
sdkman_checksum_enable=true
sdkman_healthcheck_enable=true
sdkman_native_enable=false
EOF

  probe="${HEAVY_CW_SDKMAN_JAVA:-}"
  if [[ -z "$probe" ]]; then
    probe="$("${DENY[@]}" curl -fsS "$api/candidates/default/java" || true)"
  fi
  [[ -n "$probe" ]] || heavy_fail "sdkman: the instance named no default java version"
  heavy_log "sdk install java $probe (the JDK comes from the instance), with egress denied"

  "${DENY[@]}" SDKMAN_DIR="$dir" SDKMAN_CANDIDATES_API="$api" SDKMAN_BROKER_API="$broker" \
    bash -c '
      set -o pipefail
      export SDKMAN_DIR SDKMAN_CANDIDATES_API SDKMAN_BROKER_API
      # shellcheck disable=SC1090
      source "$SDKMAN_DIR/bin/sdkman-init.sh"
      [[ "$(type -t sdk)" == "function" ]] || { echo "sdkman-init.sh did not define sdk" >&2; exit 97; }
      sdk install java "$1"
    ' _ "$probe" >"$out" 2>&1 \
    || { cat "$out" >&2; heavy_fail "sdkman: the install failed inside the closed world"; }

  heavy_wire_after sdkman "GET /proxy/$SDKMAN_REG/sdkman/candidates/all -> 200" \
    "the candidate list did not come through the proxy"
  heavy_wire_re_after sdkman \
    "GET /proxy/$SDKMAN_REG/sdkman/broker/download/java/[^ ]*/linuxx64 -> 200" \
    "the JDK did not come through the broker route — the server did not follow the vendor's 302, or the client went straight to the CDN"

  java_bin="$dir/candidates/java/$probe/bin/java"
  [[ -x "$java_bin" ]] || heavy_fail "sdkman: no java at $java_bin after the install"
  # Single-file source launch (Java 11+): the JDK this instance served both
  # compiles and runs the program, which is more than `java -version` proves.
  mkdir -p "$HEAVY_WORK/sdkman/src"
  cat >"$HEAVY_WORK/sdkman/src/Ran.java" <<'EOF'
public class Ran {
    public static void main(String[] args) {
        System.out.println("CLOSED-WORLD-RAN " + (6 * 7));
    }
}
EOF
  cw_step "$out" "$HEAVY_WORK/sdkman/src" "${DENY[@]}" "$java_bin" Ran.java \
    || { cat "$out" >&2; heavy_fail "sdkman: the installed JDK did not compile and run the program"; }
  cw_ran sdkman "$out" "42"
  heavy_log "CLOSED-WORLD-SDKMAN-OK (java $probe from the instance compiled and ran it)"
}

# ── §13. ovsx ────────────────────────────────────────────────────────────────
#
# Open VSX's own client, against the Open VSX REST API (`/api/{ns}/{ext}` and
# the `files.download` URL out of it) — not the VS Code gallery protocol, which
# §14 drives against the other extension kind. The two registry kinds share
# `require_vsx` and therefore share the gallery routes; what tells them apart
# is the upstream and the API shape a native client speaks, so each phase
# drives the client that is native to its kind.
#
# `ovsx` itself is installed *through this instance's npm registry*, which is
# not decoration: a closed world has no npmjs.org, so the only way the client
# exists at all is that the proxy served it. The one thing the extension is not
# is a program, so what is asserted at the end is that the package the instance
# served, opened, is the one that was asked for.

phase_ovsx() {
  heavy_need npm "nodejs"
  heavy_need node "nodejs"
  heavy_need unzip "unzip"
  local out dir="$HEAVY_WORK/ovsx" ns name
  out="$(cw_out ovsx)"
  ns="${OVSX_EXT%%.*}"
  name="${OVSX_EXT#*.}"
  mkdir -p "$dir"
  cat >"$dir/.npmrc" <<EOF
registry=$HEAVY_TAP_BASE/proxy/$NPM_REG/
audit=false
fund=false
update-notifier=false
EOF

  heavy_mark ovsx
  heavy_log "npm install ovsx@$OVSX_VERSION (the client comes from the instance too), with egress denied"
  cw_step "$out" "$dir" "${DENY[@]}" npm_config_cache="$HEAVY_WORK/ovsx-npm-cache" \
    npm install --no-audit --no-fund --no-save "ovsx@$OVSX_VERSION" \
    || { cat "$out" >&2; heavy_fail "ovsx: the client could not be installed from the proxied npm registry"; }
  heavy_wire_after ovsx "GET /proxy/$NPM_REG/ovsx -> 200" \
    "npm did not read ovsx's packument through the proxy"
  [[ -x "$dir/node_modules/.bin/ovsx" ]] || heavy_fail "ovsx: no ovsx binary after the install"

  heavy_log "ovsx get $OVSX_EXT through the proxy"
  cw_step "$out" "$dir" "${DENY[@]}" \
    "$dir/node_modules/.bin/ovsx" get "$OVSX_EXT" \
    --registryUrl "$HEAVY_TAP_BASE/proxy/$OVSX_REG" -o "$dir/ext.vsix" \
    || { cat "$out" >&2; heavy_fail "ovsx: the download failed inside the closed world"; }

  heavy_wire_re_after ovsx "GET /proxy/$OVSX_REG/api/${ns}/${name}[^ ]* -> 200" \
    "ovsx did not resolve the extension through this instance's Open VSX API"
  # Two requests, the second reached by following the `files.download` URL out
  # of the first: unrewritten, that URL sends the client to open-vsx.org for
  # the bytes, which in a closed world is a failure and in an open one is a
  # silent bypass of every rule on the way (RFC 0009 §12, the Terraform hole).
  heavy_wire_re_after ovsx "GET /proxy/$OVSX_REG/api/${ns}/${name}/[^ ]*/file/[^ ]* -> 200" \
    "the VSIX bytes did not come through the proxy — the files.download URL was not repointed at this instance"

  [[ -s "$dir/ext.vsix" ]] || heavy_fail "ovsx: the download wrote no file"
  head -c 2 "$dir/ext.vsix" | grep -q "PK" \
    || heavy_fail "ovsx: what came back is not a ZIP, so it is not a VSIX"
  # The identity is read out of the package the instance served, by node, so
  # the banner comes from the artifact rather than from this script. A file
  # rather than `node -e`: the quoting of a JS one-liner nested inside a
  # `bash -c` inside a heredoc is its own source of bugs.
  unzip -p "$dir/ext.vsix" extension/package.json >"$dir/package.json" 2>"$out" \
    || { cat "$out" >&2; heavy_fail "ovsx: the downloaded VSIX has no extension/package.json"; }
  cat >"$dir/identity.js" <<'JS'
const fs = require("fs");
const p = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
console.log("CLOSED-WORLD-RAN", p.publisher + "." + p.name);
JS
  cw_step "$out" "$dir" "${DENY[@]}" node identity.js package.json \
    || { cat "$out" >&2; heavy_fail "ovsx: the downloaded VSIX could not be opened"; }
  cw_ran ovsx "$out" "$OVSX_EXT"
  heavy_log "CLOSED-WORLD-OVSX-OK ($OVSX_EXT fetched and opened, client and extension both from the instance)"
}
# ── §14. VS Code ─────────────────────────────────────────────────────────────
#
# The editor as the client, and the gallery protocol as the wire: with
# `product.json` pointed here, `--install-extension <id>` resolves the
# extension through `extensionsGallery.serviceUrl` — an `extensionquery` POST
# and then an asset fetch — which is what a developer configuring BatleHub as
# their marketplace actually does. `tests/heavy/marketplace.sh` proves that
# against a locally published extension; this proves it against the *upstream*
# marketplace, with the editor unable to reach it.
#
# The server build (`server-linux-x64-web`), not the desktop one: the same
# `ExtensionManagementCLI` under the node it bundles, with no Electron and so
# no xvfb on the runner. Since 1.136 the CLI verifies signatures on install and
# refuses anything the Microsoft marketplace did not sign, so
# `extensions.verifySignature` is turned off in the profile — RFC 0020 §4.5:
# the one lever a stock build has, and the one VSCodium and code-server ship
# off by default.

phase_vscode() {
  local out dir="$HEAVY_WORK/vscode" vscode_dir code_server product
  out="$(cw_out vscode)"
  vscode_dir="$(heavy_cached_dir "vscode-server-web-$VSCODE_VERSION" \
    "https://update.code.visualstudio.com/$VSCODE_VERSION/server-linux-x64-web/stable" tar.gz)"

  # The cached build is **copied** into the run before anything in it is
  # changed, rather than patched in place and restored afterwards.
  # `product.json` is the one file this phase has to rewrite, and that cache is
  # shared with `marketplace.sh`, `vsx_login.sh` and `vsx_view.sh`: a
  # restore-on-exit leaves the editor pointed at a dead port for every later
  # suite on that runner the moment this phase fails, because `heavy_fail`
  # exits rather than returning. A few hundred megabytes of `cp` buys the
  # guarantee that it cannot.
  mkdir -p "$dir/build"
  cp -a "$vscode_dir/." "$dir/build/" \
    || heavy_fail "vscode: could not copy the cached build into the run"
  # The tarball carries one top-level directory; `heavy_cached_dir` does not
  # strip it, so the build root is whatever single entry landed inside.
  code_server="$(find "$dir/build" -maxdepth 3 -type f -name code-server -perm -u+x | head -1)"
  [[ -n "$code_server" ]] || heavy_fail "vscode: no bin/code-server under $dir/build"
  product="$(dirname "$(dirname "$code_server")")/product.json"
  [[ -f "$product" ]] || heavy_fail "vscode: no product.json beside $code_server"

  mkdir -p "$dir/server/data/User" "$dir/user" "$dir/extensions"
  echo '{ "extensions.verifySignature": false }' >"$dir/server/data/User/settings.json"

  BASE="$HEAVY_TAP_BASE" REGISTRY="$VSCODE_REG" python3 tests/heavy/patch_product_json.py "$product" >"$out" 2>&1 \
    || { cat "$out" >&2; heavy_fail "vscode: could not point product.json at this instance"; }

  heavy_mark vscode
  heavy_log "code-server --install-extension $VSCODE_EXT by id, through the gallery, with egress denied"
  # `env -u VSCODE_IPC_HOOK_CLI`: run from a terminal *inside* VS Code, the CLI
  # forwards every command to that editor over its IPC socket — the install
  # would land in the developer's own editor, against the developer's own
  # gallery, and pass while proving nothing about this instance.
  cw_step "$out" "$dir" "${DENY[@]}" env -u VSCODE_IPC_HOOK_CLI \
    "$code_server" --server-data-dir "$dir/server" --user-data-dir "$dir/user" \
    --extensions-dir "$dir/extensions" --install-extension "$VSCODE_EXT" --force \
    || { cat "$out" >&2; heavy_fail "vscode: the gallery install failed inside the closed world"; }

  heavy_wire_after vscode "POST /proxy/$VSCODE_REG/vscode/gallery/extensionquery -> 200" \
    "the editor did not resolve the extension through this instance's gallery"
  heavy_wire_re_after vscode "GET /proxy/$VSCODE_REG/vscode/(asset|unpkg)/[^ ]* -> 200" \
    "the VSIX bytes did not come through the proxy"

  cw_step "$out" "$dir" "${DENY[@]}" env -u VSCODE_IPC_HOOK_CLI \
    "$code_server" --server-data-dir "$dir/server" --user-data-dir "$dir/user" \
    --extensions-dir "$dir/extensions" --list-extensions \
    || { cat "$out" >&2; heavy_fail "vscode: the editor could not list its extensions"; }
  grep -qix "$VSCODE_EXT" "$out" || {
    cat "$out" >&2
    heavy_fail "vscode: $VSCODE_EXT is not listed after the install"
  }
  heavy_log "CLOSED-WORLD-VSCODE-OK ($VSCODE_EXT installed by id and listed back by the editor)"
}

# ── §15. JetBrains Marketplace ───────────────────────────────────────────────
#
# IntelliJ's headless `installPlugins`, with the marketplace host pointed here.
# `updatePlugins.xml` — the route `tests/heavy/marketplace.sh` uses — is a
# *local*-mode document and answers `404` on a proxy registry, so what this
# phase drives instead is the IDE-facing API the stock editor uses:
# `idea.plugins.host`, which is the one property that moves the whole flow
# (search, compatible-updates, download) onto another host.
#
# The IDE archive is ~1.5 GB and is cached across runs; the *plugin* is what
# crosses the proxy. `HEAVY_CW_SKIP_JB=1` leaves this row unmeasured on a
# runner that cannot spare the disk, and says so in the banner rather than
# passing quietly.

phase_jbplugin() {
  if [[ "${HEAVY_CW_SKIP_JB:-0}" == "1" ]]; then
    heavy_log "CLOSED-WORLD-JBPLUGIN-SKIPPED (HEAVY_CW_SKIP_JB=1 — the jetbrains-marketplace row is NOT measured)"
    return 0
  fi
  local out dir="$HEAVY_WORK/jbplugin" idea_dir downloads
  out="$(cw_out jbplugin)"
  # Unified installer naming since 2025.3; the Community tarball is the
  # fallback for an override pinning an older IDEA_VERSION.
  if ! idea_dir="$(heavy_cached_dir "idea-$IDEA_VERSION" \
      "https://download.jetbrains.com/idea/idea-$IDEA_VERSION.tar.gz" tar.gz 2>/dev/null)"; then
    idea_dir="$(heavy_cached_dir "ideaIC-$IDEA_VERSION" \
      "https://download.jetbrains.com/idea/ideaIC-$IDEA_VERSION.tar.gz" tar.gz)"
  fi
  local idea_sh
  idea_sh="$(find "$idea_dir" -maxdepth 3 -type f -name idea.sh | head -1)"
  [[ -n "$idea_sh" ]] || heavy_fail "jbplugin: no bin/idea.sh under $idea_dir"

  mkdir -p "$dir"/{config,system,plugins,log}
  # `idea.plugins.host` is the one property that moves the whole plugin flow —
  # search, compatible updates, download — onto another host, and
  # `idea.properties` is read into the system properties at startup, which is
  # why it can live beside the path redirections rather than in a vmoptions
  # file the launcher would also have to be told about.
  cat >"$dir/idea.properties" <<EOF
idea.config.path=$dir/config
idea.system.path=$dir/system
idea.plugins.path=$dir/plugins
idea.log.path=$dir/log
idea.plugins.host=$HEAVY_TAP_BASE/proxy/$JBM_REG
EOF

  heavy_mark jbplugin
  heavy_log "IntelliJ installPlugins $JB_PLUGIN (marketplace host = this instance), with egress denied"
  cw_step "$out" "$dir" "${DENY[@]}" \
    IDEA_PROPERTIES="$dir/idea.properties" \
    "$idea_sh" installPlugins "$JB_PLUGIN" \
    || { cat "$out" >&2; heavy_fail "jbplugin: installPlugins failed inside the closed world"; }

  # Either of the two canonical download URLs is the proof; which one the IDE
  # picks is a function of its build, not of this instance, so the assertion
  # covers both rather than pinning the one a single IDEA version happens to use.
  heavy_wire_re_after jbplugin \
    "GET /proxy/$JBM_REG/(pluginManager|plugin/download)[^ ]* -> 200" \
    "the plugin did not come through this instance — idea.plugins.host did not take"

  downloads="$(heavy_wire_count_after jbplugin "GET /proxy/$JBM_REG/[^ ]* -> 200")"
  [[ -n "$(ls -A "$dir/plugins" 2>/dev/null)" ]] \
    || { cat "$out" >&2; heavy_fail "jbplugin: no plugin was installed into $dir/plugins"; }
  find "$dir/plugins" -maxdepth 2 -iname "*${JB_PLUGIN_DIR_MATCH}*" | grep -q . \
    || { find "$dir/plugins" -maxdepth 2 >&2; heavy_fail "jbplugin: the installed plugins directory does not contain $JB_PLUGIN"; }
  heavy_log "CLOSED-WORLD-JBPLUGIN-OK ($JB_PLUGIN installed from this instance, $downloads requests through the proxy)"
}

# ── §16. JetBrains downloads ─────────────────────────────────────────────────
#
# `jetbrains`, the path-proxy kind: no metadata API at all, a plain file tree
# addressed by path. The file fetched is the JetBrains Runtime rather than an
# IDE archive — a JBR is ~100 MB against an IDE's ~1.5 GB, and it is a JDK, so
# the phase still ends in a program this instance served being executed. An IDE
# archive would also need `[limits].max_artifact_size_bytes` raised above its
# 500 MiB default, since `ProxyService::handle` buffers the whole artifact.
#
# `curl` is the client, and that is not a weakness of the phase but the shape
# of the kind: a path proxy has no protocol for a package manager to speak, so
# what has to be true is that the bytes arrive addressed by path and what they
# contain runs. The upstream answers a cross-host `302` to its CDN, which the
# server follows (SSRF-validated per hop) and the client never sees.

phase_jbr() {
  heavy_need curl "curl"
  local out dir="$HEAVY_WORK/jbr" java_bin
  out="$(cw_out jbr)"
  mkdir -p "$dir/unpacked" "$dir/src"

  heavy_mark jbr
  heavy_log "Fetching $JBR_PATH by path through the proxy, with egress denied"
  "${DENY[@]}" curl -fsS --max-time 900 \
    "$HEAVY_TAP_BASE/proxy/$JB_REG/jetbrains/$JBR_PATH" -o "$dir/jbr.tar.gz" \
    >"$out" 2>&1 \
    || { cat "$out" >&2; heavy_fail "jbr: the download failed inside the closed world"; }
  heavy_wire_after jbr "GET /proxy/$JB_REG/jetbrains/$JBR_PATH -> 200" \
    "the JBR did not come through the proxy"
  [[ -s "$dir/jbr.tar.gz" ]] || heavy_fail "jbr: the download wrote no file"

  tar -xzf "$dir/jbr.tar.gz" -C "$dir/unpacked" --strip-components=1 \
    || heavy_fail "jbr: what came back does not unpack as a gzip tarball"
  java_bin="$dir/unpacked/bin/java"
  [[ -x "$java_bin" ]] || heavy_fail "jbr: no bin/java in the unpacked runtime"

  cat >"$dir/src/Ran.java" <<'EOF'
public class Ran {
    public static void main(String[] args) {
        System.out.println("CLOSED-WORLD-RAN " + (6 * 7));
    }
}
EOF
  cw_step "$out" "$dir/src" "${DENY[@]}" "$java_bin" Ran.java \
    || { cat "$out" >&2; heavy_fail "jbr: the runtime this instance served did not run the program"; }
  cw_ran jbr "$out" "42"
  heavy_log "CLOSED-WORLD-JBR-OK ($JBR_PATH fetched by path and executed)"
}

# ── §17. Forgejo ─────────────────────────────────────────────────────────────
#
# mise's `forgejo:` backend against Codeberg. The release *routes* are shared
# with GitHub (`require_github` accepts both kinds), so what this phase adds
# over §10 is the upstream API shape: Forgejo answers
# `/api/v1/repos/{owner}/{repo}/releases`, which is not GitHub's, and the
# adapter is what makes one look like the other to a client.
#
# One rewrite rule is enough, and that is the load-bearing observation: the
# release document this instance serves has every `browser_download_url`
# repointed at the proxy (RFC 0019 §4.2), so a client that *follows* the
# document rather than building a path is carried through by the listing
# alone. Codeberg serves its attachments from `/attachments/{uuid}`, a host
# path this instance has no route for — so if the repointing ever stopped, the
# closed world would stop this phase rather than let it walk out.

phase_forgejo() {
  local out proxy="$HEAVY_TAP_BASE/proxy/$FORGEJO_REG"
  out="$(cw_out forgejo)"
  heavy_runner_for mise "mise@latest"
  local mise=("${HEAVY_RUNNER[@]}" mise)
  local root="$HEAVY_WORK/forgejo-mise"
  mkdir -p "$root"/{data,cache,config,state}
  cat >"$root/config/config.toml" <<EOF
[settings]
# The attestation check §10 records is a GitHub-only flow; off here for the
# same reason, so a refusal cannot be mistaken for one about this backend.
github_attestations = false

[settings.url_replacements]
"regex:^https://codeberg\\\\.org/api/v1/repos/(.+)" = "$proxy/\$1"
# The asset download is not the browser_download_url in the release document:
# mise's forgejo backend builds {forge}/attachments/{uuid} from its *own*
# configured api_url and downloads from that, so the rewritten document routes
# only the checksum sibling and the binary goes straight to codeberg - which,
# inside the closed world, is nowhere. The second rule is the asset itself.
# (No backticks: this heredoc is unquoted, so they would run as commands.)
"regex:^https://codeberg\\\\.org/attachments/(.+)" = "$proxy/attachments/\$1"
EOF
  local miseenv=(MISE_DATA_DIR="$root/data" MISE_CACHE_DIR="$root/cache"
                 MISE_CONFIG_DIR="$root/config" MISE_STATE_DIR="$root/state" MISE_YES=1)

  heavy_mark forgejo
  heavy_log "mise install forgejo:$FORGEJO_TOOL@$FORGEJO_TOOL_VERSION, with egress denied"
  cw_step "$out" "$HEAVY_WORK" "${DENY[@]}" "${miseenv[@]}" \
    "${mise[@]}" install "forgejo:$FORGEJO_TOOL@$FORGEJO_TOOL_VERSION" \
    || { cat "$out" >&2; heavy_fail "forgejo: the install failed inside the closed world"; }

  heavy_wire_re_after forgejo "GET /proxy/$FORGEJO_REG/$FORGEJO_TOOL/releases[^ ]* -> 200" \
    "mise did not resolve the release listing through the proxy"
  # The checksum sibling, which mise does fetch by the document's own URL: what
  # this asserts is the rewrite.
  heavy_wire_re_after forgejo \
    "GET /proxy/$FORGEJO_REG/$FORGEJO_TOOL/releases/download/[^ ]*\\.sha256 -> 200" \
    "the release document's download URLs were not repointed at this instance"
  # The asset itself, by the uuid the document named it with (RFC 0019 §4.2).
  heavy_wire_re_after forgejo \
    "GET /proxy/$FORGEJO_REG/attachments/[^ ]* -> 200" \
    "the release asset did not come through the proxy — the attachment route did not serve it"

  cw_step "$out" "$HEAVY_WORK" "${DENY[@]}" "${miseenv[@]}" \
    "${mise[@]}" exec "forgejo:$FORGEJO_TOOL@$FORGEJO_TOOL_VERSION" -- "$FORGEJO_TOOL_BIN" --version \
    || { cat "$out" >&2; heavy_fail "forgejo: the installed tool did not run"; }
  grep -q "$FORGEJO_TOOL_VERSION" "$out" || {
    cat "$out" >&2
    heavy_fail "forgejo: the tool that ran is not the version this instance served"
  }
  heavy_log "CLOSED-WORLD-FORGEJO-OK ($(grep -m1 -i version "$out"))"
}
# ── §18. GitLab ──────────────────────────────────────────────────────────────
#
# mise's `gitlab:` backend. The rewrite sends the backend's API call at the
# *typed* route (`{project}/-/releases`) rather than at the `api/v4`
# passthrough beside it, and the difference is the whole phase: the typed route
# renders the document through `proxy_document`, which repoints every asset URL
# at this instance, while the passthrough streams GitLab's own bytes with
# GitLab's own URLs inside — from which a client in a closed world goes
# nowhere. The project is percent-encoded by the backend (`group%2Fproject`)
# and decoded by actix into the `{project:.+}` parameter, so the encoded form
# is carried through the rewrite unchanged.

phase_gitlab() {
  local out proxy="$HEAVY_TAP_BASE/proxy/$GITLAB_REG"
  out="$(cw_out gitlab)"
  heavy_runner_for mise "mise@latest"
  local mise=("${HEAVY_RUNNER[@]}" mise)
  local root="$HEAVY_WORK/gitlab-mise"
  mkdir -p "$root"/{data,cache,config,state}
  cat >"$root/config/config.toml" <<EOF
[settings]
github_attestations = false

[settings.url_replacements]
"regex:^https://gitlab\\\\.com/api/v4/projects/([^/]+)/releases(.*)" = "$proxy/\$1/-/releases\$2"
EOF
  local miseenv=(MISE_DATA_DIR="$root/data" MISE_CACHE_DIR="$root/cache"
                 MISE_CONFIG_DIR="$root/config" MISE_STATE_DIR="$root/state" MISE_YES=1)

  heavy_mark gitlab
  heavy_log "mise install gitlab:$GITLAB_TOOL@$GITLAB_TOOL_VERSION, with egress denied"
  cw_step "$out" "$HEAVY_WORK" "${DENY[@]}" "${miseenv[@]}" \
    "${mise[@]}" install "gitlab:$GITLAB_TOOL@$GITLAB_TOOL_VERSION" \
    || { cat "$out" >&2; heavy_fail "gitlab: the install failed inside the closed world"; }

  # The typed route, by name: if the rewrite ever lands on the `api/v4`
  # passthrough instead, this is the assertion that says so rather than a
  # confusing download failure three lines later.
  heavy_wire_re_after gitlab "GET /proxy/$GITLAB_REG/[^ ]*/-/releases[^ ]* -> 200" \
    "mise did not resolve the release listing through the typed GitLab route"
  heavy_wire_re_after gitlab "GET /proxy/$GITLAB_REG/[^ ]* -> 200" \
    "nothing at all came through the proxy"

  cw_step "$out" "$HEAVY_WORK" "${DENY[@]}" "${miseenv[@]}" \
    "${mise[@]}" exec "gitlab:$GITLAB_TOOL@$GITLAB_TOOL_VERSION" -- "$GITLAB_TOOL_BIN" --version \
    || { cat "$out" >&2; heavy_fail "gitlab: the installed tool did not run"; }
  grep -q "$GITLAB_TOOL_VERSION" "$out" || {
    cat "$out" >&2
    heavy_fail "gitlab: the tool that ran is not the version this instance served"
  }
  heavy_log "CLOSED-WORLD-GITLAB-OK ($(grep -m1 -i version "$out"))"
}

# ── §19. helm ────────────────────────────────────────────────────────────────
#
# `generic`, the kind for an upstream with no package protocol at all: a plain
# file tree, addressed by path, with `path_allow` as the only thing standing
# between the registry and every other path on that host. The client is
# therefore whatever fetches by path — here `curl`, because that is what helm's
# own installer is — and the proof is on the far side of the download: the
# binary this instance served renders a chart.
#
# `helm template` rather than `helm version`: rendering is helm doing its job,
# and it needs no cluster, no network and no configuration to do it.

phase_helm() {
  heavy_need curl "curl"
  local out dir="$HEAVY_WORK/helm" tarball="helm-v$HELM_VERSION-linux-amd64.tar.gz" helm_bin
  out="$(cw_out helm)"
  mkdir -p "$dir/unpacked" "$dir/chart/templates"

  heavy_mark helm
  heavy_log "Fetching $tarball by path through the proxy, with egress denied"
  "${DENY[@]}" curl -fsS --max-time 300 \
    "$HEAVY_TAP_BASE/proxy/$HELM_REG/generic/$tarball" -o "$dir/helm.tar.gz" \
    >"$out" 2>&1 \
    || { cat "$out" >&2; heavy_fail "helm: the download failed inside the closed world"; }
  heavy_wire_after helm "GET /proxy/$HELM_REG/generic/$tarball -> 200" \
    "the helm archive did not come through the proxy"

  tar -xzf "$dir/helm.tar.gz" -C "$dir/unpacked" --strip-components=1 \
    || heavy_fail "helm: what came back does not unpack as a gzip tarball"
  helm_bin="$dir/unpacked/helm"
  [[ -x "$helm_bin" ]] || heavy_fail "helm: no helm binary in the unpacked archive"

  cat >"$dir/chart/Chart.yaml" <<'EOF'
apiVersion: v2
name: closedworld
version: 1.0.0
EOF
  cat >"$dir/chart/values.yaml" <<'EOF'
answer: 42
EOF
  cat >"$dir/chart/templates/ran.yaml" <<'EOF'
apiVersion: v1
kind: ConfigMap
metadata:
  name: closed-world
data:
  ran: "CLOSED-WORLD-RAN {{ .Values.answer }}"
EOF

  cw_step "$out" "$dir" "${DENY[@]}" "$helm_bin" template closedworld ./chart \
    || { cat "$out" >&2; heavy_fail "helm: the binary this instance served did not render the chart"; }
  cw_ran helm "$out" "42"
  heavy_log "CLOSED-WORLD-HELM-OK (helm v$HELM_VERSION fetched by path and rendered a chart)"
}

# ── The three OS package managers ────────────────────────────────────────────
#
# `deb`, `rpm` and `pacman` are the family whose indexes are signed upstream and
# which BatleHub therefore cannot filter (RFC 0018 §4.4 — `tests/heavy/
# pathproxy.sh` measures the refusal half). What a closed world adds is the
# other half: the package still *arrives*, and what is in it runs, with the
# distribution's archive unreachable.
#
# **Two of the three run their client inside that distribution's own image, and
# the reason is not convenience.** A distribution package is dynamically linked
# against *its own* libraries: Arch's `jq` wants Arch's `libonig.so.5`, and an
# EPEL build wants EPEL's. Downloading such a package on Ubuntu and executing
# what is inside it cannot work, so a phase written that way could only ever
# pass on a host that already was that distribution — which for `pacman` means
# essentially no CI runner in existence. Running the client where the package
# was built is the only correct form of the claim, and it has a second payoff:
# the runner no longer has to be persuaded to host a foreign package manager,
# which is where the `rpm` row's fragility used to live.
#
# `deb` is the exception and stays on the host: the runner *is* Ubuntu, so its
# client is native and the package's binary runs. A container would add a
# dependency and prove nothing more.
#
# **The container is not the isolation.** With `--network host` — which is what
# keeps `127.0.0.1:<tap>` meaning the same thing inside as out — the container
# shares the host's network namespace, so egress is denied exactly as it is
# everywhere else in this file: by the six proxy variables, passed in with `-e`.
# Each containerised phase re-proves that denial from inside, against a real
# host, before it trusts anything else — §0 establishes it for a *host* process
# and cannot speak for a different process in a different filesystem.
#
# All three run with their state inside the run — on the host, redirected
# `Dir::` options and no root; in a container, a `--rm` that takes the state
# with it.

# ── §20. apt ─────────────────────────────────────────────────────────────────

phase_apt() {
  heavy_need apt-get "apt"
  heavy_need dpkg "dpkg"
  local out root="$HEAVY_WORK/apt" arch deb_url deb_path
  out="$(cw_out apt)"
  arch="$(dpkg --print-architecture)"
  [[ -r "$APT_KEYRING" ]] || heavy_fail "apt: no archive keyring at $APT_KEYRING — set HEAVY_CW_APT_KEYRING"
  mkdir -p "$root/state/lists/partial" "$root/cache/archives/partial" "$root/out" "$root/x"
  local apt_url="$HEAVY_TAP_BASE/proxy/$DEB_REG/deb"
  echo "deb [arch=$arch signed-by=$APT_KEYRING] $apt_url $APT_SUITE main" >"$root/sources.list"

  # apt, with every `Dir::` redirected into the run. `Debug::NoLocking` is what
  # lets it work without write access to /var/lib/dpkg.
  local apt=(apt-get -q
    -o "Dir::State=$root/state" -o "Dir::Cache=$root/cache"
    -o "Dir::Etc::SourceList=$root/sources.list" -o "Dir::Etc::SourceParts=/dev/null"
    -o "Dir::Etc::Preferences=/dev/null" -o "Dir::Etc::PreferencesParts=/dev/null"
    -o "Dir::Etc::Main=/dev/null" -o "Dir::Etc::Parts=/dev/null"
    -o "Acquire::Languages=none" -o "Debug::NoLocking=true")

  heavy_mark apt
  heavy_log "apt-get update + download $APT_PACKAGE, with egress denied"
  cw_step "$out" "$root/out" "${DENY[@]}" "${apt[@]}" update \
    || { cat "$out" >&2; heavy_fail "apt: update through the proxy failed inside the closed world"; }
  heavy_wire_after apt "GET /proxy/$DEB_REG/deb/dists/$APT_SUITE/InRelease -> 200" \
    "apt did not fetch the signed InRelease through the proxy"

  # The path apt will ask for, from apt itself rather than from a guess.
  cw_step "$out" "$root/out" "${DENY[@]}" "${apt[@]}" download --print-uris "$APT_PACKAGE" \
    || { cat "$out" >&2; heavy_fail "apt: --print-uris failed"; }
  deb_url="$(grep -o "'[^']*\.deb'" "$out" | head -1 | tr -d "'" || true)"
  [[ -n "$deb_url" ]] || { cat "$out" >&2; heavy_fail "apt: printed no .deb URI for $APT_PACKAGE"; }
  deb_path="${deb_url#"$apt_url"/}"

  cw_step "$out" "$root/out" "${DENY[@]}" "${apt[@]}" download "$APT_PACKAGE" \
    || { cat "$out" >&2; heavy_fail "apt: the download failed inside the closed world"; }
  heavy_wire_after apt "GET /proxy/$DEB_REG/deb/$deb_path -> 200" \
    "the .deb did not come through the proxy"

  local deb_file
  deb_file="$(ls "$root/out"/*.deb 2>/dev/null | head -1 || true)"
  [[ -n "$deb_file" ]] || heavy_fail "apt: reported success but wrote no .deb"
  dpkg -x "$deb_file" "$root/x" || heavy_fail "apt: the .deb could not be unpacked"
  [[ -x "$root/x/$APT_PACKAGE_BIN" ]] \
    || heavy_fail "apt: no $APT_PACKAGE_BIN in the unpacked package"

  "${DENY[@]}" "$root/x/$APT_PACKAGE_BIN" >"$root/ran.txt" 2>&1 \
    || { cat "$root/ran.txt" >&2; heavy_fail "apt: the packaged binary did not run"; }
  cw_ran_from apt "$out" "$APT_PACKAGE_RAN" "$root/ran.txt"
  heavy_log "CLOSED-WORLD-APT-OK ($deb_path fetched and executed)"
}

# ── §21. dnf ─────────────────────────────────────────────────────────────────
#
# AlmaLinux 9, because the registry's upstream is EPEL 9 and EPEL 9 targets the
# RHEL 9 line: a package built there installs and runs in that userland and in
# no other. The base repositories are *removed* rather than merely disabled, so
# `dnf` has this instance and nothing else to resolve from; the package's own
# dependencies come from what the image already has installed, which is why a
# dependency-light package is chosen and why `install_weak_deps` is off.
#
# `dnf install` rather than `dnf download` plus a hand-unpack: inside the
# container this phase is root, so the client can do the thing it exists to do,
# and what runs afterwards is an installed program rather than a file tree.

phase_dnf() {
  if [[ "${HEAVY_CW_SKIP_RPM:-0}" == "1" ]]; then
    heavy_log "CLOSED-WORLD-DNF-SKIPPED (HEAVY_CW_SKIP_RPM=1 — the rpm row is NOT measured)"
    return 0
  fi
  heavy_container_engine
  local out base denv script
  out="$(cw_out dnf)"
  base="$HEAVY_TAP_BASE/proxy/$RPM_REG/rpm"
  mapfile -t denv < <(cw_container_deny)

  heavy_mark dnf
  # The image is the *client*, and a client is obtained before the world closes
  # — exactly as nvm, sdkman and the editors are fetched outside the denial.
  heavy_log "Pulling $RPM_IMAGE (the client, before the world closes)"
  "${CW_ENGINE[@]}" pull "$RPM_IMAGE" >"$out" 2>&1 \
    || { cat "$out" >&2; heavy_fail "dnf: could not pull $RPM_IMAGE"; }

  script="$(cat <<'SH'
set -eu
base="$1"; pkg="$2"; cmd="$3"; probe="$4"
command -v curl >/dev/null 2>&1 || { echo "no curl in the image: the egress control cannot be run" >&2; exit 96; }
# §0 proves the denial for a host process; this proves it for *this* process,
# in this image. Without it a container that ignored the variables would make
# every assertion below pass for the wrong reason.
# As §0 on the host: the denial answers `403 closed world: …`, which `curl`
# exits 0 on, so the body is what says whether this container is closed.
probe_status="$(curl -sS --max-time 20 -w '%{http_code}' -o /tmp/egress.body "$probe")" || probe_status=unreachable
case "$probe_status" in
  403) grep -q "closed world" /tmp/egress.body || { echo "the container got a 403 from something other than the closed-world proxy" >&2; exit 98; } ;;
  unreachable) ;;
  *) echo "the container reached $probe ($probe_status) — the world is not closed in here" >&2; exit 98 ;;
esac
rm -f /etc/yum.repos.d/*.repo
cat >/etc/yum.repos.d/closed-world.repo <<EOF
[closed-world]
name=closed-world
baseurl=$base
enabled=1
gpgcheck=0
EOF
dnf -y --disablerepo='*' --enablerepo=closed-world \
    --setopt=install_weak_deps=False install "$pkg"
echo "CLOSED-WORLD-RAN $("$cmd" --version 2>&1 | head -1)"
SH
)"

  heavy_log "dnf install $DNF_PACKAGE inside $RPM_IMAGE, with egress denied"
  "${CW_ENGINE[@]}" run --rm --network host "${denv[@]}" "$RPM_IMAGE" \
    bash -c "$script" _ "$base" "$DNF_PACKAGE" "$DNF_PACKAGE_CMD" \
    "https://dl.fedoraproject.org/pub/epel/9/Everything/x86_64/repodata/repomd.xml" \
    >"$out" 2>&1 \
    || { cat "$out" >&2; heavy_fail "dnf: the install failed inside the closed world"; }

  heavy_wire_re_after dnf "GET /proxy/$RPM_REG/rpm/[^ ]*repomd.xml -> 200" \
    "dnf did not read the repository metadata through the proxy"
  heavy_wire_re_after dnf "GET /proxy/$RPM_REG/rpm/[^ ]*[.]rpm -> 200" \
    "the .rpm did not come through the proxy"
  cw_ran dnf "$out" "$DNF_PACKAGE_RAN"
  heavy_log "CLOSED-WORLD-DNF-OK ($DNF_PACKAGE installed from this instance in $RPM_IMAGE and executed)"
}

# ── §22. pacman ──────────────────────────────────────────────────────────────
#
# Arch, in Arch's own image, for the reason above and one more: `pacman` cannot
# be apt-installed and no hosted runner ships it, so this kind's closed world
# was simply unmeasured before. `pacman.conf` is replaced rather than edited —
# the stock file carries an `Include` of the mirrorlist, and a registry that is
# merely *first* in a list still containing the internet is not a closed world.
#
# `SigLevel = Never`, deliberately. Arch's packages are signed and the image
# carries a keyring, so verification would normally pass and would be evidence
# the bytes arrived unaltered — but it would also make this phase fail whenever
# that keyring lagged a key rotation, which is a red that says nothing about
# BatleHub. What this family's rows measure is that the package arrives and
# runs; the signed index is the thing RFC 0018 §4.4 records BatleHub as unable
# to filter in the first place.

phase_pacman() {
  if [[ "${HEAVY_CW_SKIP_PACMAN:-0}" == "1" ]]; then
    heavy_log "CLOSED-WORLD-PACMAN-SKIPPED (HEAVY_CW_SKIP_PACMAN=1 — the pacman row is NOT measured)"
    return 0
  fi
  heavy_container_engine
  local out base arch denv script
  out="$(cw_out pacman)"
  base="$HEAVY_TAP_BASE/proxy/$PACMAN_REG/pacman"
  arch="$(uname -m)"
  mapfile -t denv < <(cw_container_deny)

  heavy_mark pacman
  heavy_log "Pulling $PACMAN_IMAGE (the client, before the world closes)"
  "${CW_ENGINE[@]}" pull "$PACMAN_IMAGE" >"$out" 2>&1 \
    || { cat "$out" >&2; heavy_fail "pacman: could not pull $PACMAN_IMAGE"; }

  script="$(cat <<'SH'
set -eu
base="$1"; arch="$2"; pkg="$3"; cmd="$4"; probe="$5"
command -v curl >/dev/null 2>&1 || { echo "no curl in the image: the egress control cannot be run" >&2; exit 96; }
# As §0 on the host: the denial answers `403 closed world: …`, which `curl`
# exits 0 on, so the body is what says whether this container is closed.
probe_status="$(curl -sS --max-time 20 -w '%{http_code}' -o /tmp/egress.body "$probe")" || probe_status=unreachable
case "$probe_status" in
  403) grep -q "closed world" /tmp/egress.body || { echo "the container got a 403 from something other than the closed-world proxy" >&2; exit 98; } ;;
  unreachable) ;;
  *) echo "the container reached $probe ($probe_status) — the world is not closed in here" >&2; exit 98 ;;
esac
# The architecture is substituted here rather than left as pacman's own `$arch`
# so the file carries no variable at all: one less thing that can expand to
# something other than what this run measured.
cat >/etc/pacman.conf <<EOF
[options]
Architecture = $arch
SigLevel = Never

[core]
Server = $base/core/os/$arch

[extra]
Server = $base/extra/os/$arch
EOF
pacman -Sy --noconfirm "$pkg"
echo "CLOSED-WORLD-RAN $("$cmd" --version 2>&1 | head -1)"
SH
)"

  heavy_log "pacman -Sy $PACMAN_PACKAGE inside $PACMAN_IMAGE, with egress denied"
  "${CW_ENGINE[@]}" run --rm --network host "${denv[@]}" "$PACMAN_IMAGE" \
    bash -c "$script" _ "$base" "$arch" "$PACMAN_PACKAGE" "$PACMAN_PACKAGE_CMD" \
    "https://geo.mirror.pkgbuild.com/core/os/$arch/core.db" \
    >"$out" 2>&1 \
    || { cat "$out" >&2; heavy_fail "pacman: the install failed inside the closed world"; }

  heavy_wire_re_after pacman "GET /proxy/$PACMAN_REG/pacman/core/os/$arch/core.db -> 200" \
    "pacman did not read the core database through the proxy"
  heavy_wire_re_after pacman "GET /proxy/$PACMAN_REG/pacman/[^ ]*[.]pkg[.]tar[.](zst|xz) -> 200" \
    "the package did not come through the proxy"
  cw_ran pacman "$out" "$PACMAN_PACKAGE_RAN"
  heavy_log "CLOSED-WORLD-PACMAN-OK ($PACMAN_PACKAGE installed from this instance in $PACMAN_IMAGE and executed)"
}

# ── Run the phases that were asked for ───────────────────────────────────────

RAN=()
for phase in "${WANTED[@]}"; do
  heavy_log "── phase: $phase ──"
  "phase_$phase"
  RAN+=("$phase")
done

heavy_done "CLOSED-WORLD-HEAVY-OK: ${RAN[*]}"
