#!/usr/bin/env bash
# The `batlehub-vsx` extension (RFC 0011 §6.5, §12 phases 7–8, §14.11) in a
# real editor, against **this** checkout's server and CLI.
#
# The extension lives in its own repository — https://github.com/batlehub/
# batlehub-vsx, §11 q6 — and so does the suite that proves it: its
# `tests/heavy/view.sh` packages the extension from source, starts a BatleHub
# from `BATLEHUB_SRC`, and drives the VS Code web build's workbench in a
# browser over CDP, reading the views off the DOM. Both of the extension's
# modes, one run:
#
#   marketplace  a stock build (the gallery cannot be repointed), a registry
#                whose `anonymous` holds no verb, `BATLEHUB_TOKEN` in the
#                editor's environment: the BatleHub view lists what the
#                registry shows this credential; its inline Install verifies
#                RFC 0020's Ed25519 signature with the registry's key and
#                hands the VSIX to the editor's own install command; the
#                editor's Extensions view then lists it as installed.
#   broker       the gallery is `batlehub-cli proxy serve`, no credential yet:
#                Account view and status bar say sign in, the Extensions view
#                lists the sign-in entry alone; once the CLI can hand out a
#                credential the extension writes the §4.1 contract file
#                (0600, keyed by the origin) and re-queries — the same
#                workbench, no reload, lists the registry's extension.
#
# This script is the gate on *this* side: it finds (or clones) that
# repository, installs its dependencies, and runs its suite with
# `BATLEHUB_SRC` pointed here — so a change to the server's VSX API, the
# CLI's contract file or the gallery proxy is measured against the client
# that consumes them, in the editor that runs it.
#
# Environment: DATABASE_URL (required); CDP_URL (a Chrome's DevTools
# endpoint; default http://127.0.0.1:9222 — the workspace's browser sidecar,
# or `.github/actions/headless-chrome` in CI); BATLEHUB_VSX_SRC (a checkout
# of the extension repository; default ../batlehub-vsx beside this one, else
# a shallow clone under HEAVY_CACHE); BATLEHUB_VSX_REF (what to clone,
# default main); VSCODE_VERSION (1.136.2); HEAVY_ONLY=marketplace|broker.
# Ports are the extension suite's own: 8124 (server), 8132 (editor).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
HEAVY_CACHE="${HEAVY_CACHE:-$HOME/.cache/batlehub-heavy}"
CDP_URL="${CDP_URL:-http://127.0.0.1:9222}"
VSX_REPO_URL="${BATLEHUB_VSX_URL:-https://github.com/batlehub/batlehub-vsx}"
VSX_REF="${BATLEHUB_VSX_REF:-main}"

log() { printf '\n==> %s\n' "$*"; }
fail() { echo "ERROR: $*" >&2; exit 1; }

: "${DATABASE_URL:?DATABASE_URL must point at a reachable Postgres}"
curl -sf "$CDP_URL/json/version" >/dev/null \
  || fail "no browser at $CDP_URL — set CDP_URL to a Chrome's DevTools endpoint (the sidecar's 9222, or .github/actions/headless-chrome)"

# ── 0. The extension repository ─────────────────────────────────────────────

VSX_SRC="${BATLEHUB_VSX_SRC:-}"
if [[ -z "$VSX_SRC" && -f "$ROOT/../batlehub-vsx/tests/heavy/view.sh" ]]; then
  VSX_SRC="$(cd "$ROOT/../batlehub-vsx" && pwd)"
fi
if [[ -z "$VSX_SRC" ]]; then
  VSX_SRC="$HEAVY_CACHE/batlehub-vsx"
  if [[ -d "$VSX_SRC/.git" ]]; then
    log "Updating $VSX_SRC to $VSX_REF"
    git -C "$VSX_SRC" fetch --depth 1 origin "$VSX_REF" && git -C "$VSX_SRC" checkout -q FETCH_HEAD
  else
    log "Cloning $VSX_REPO_URL ($VSX_REF) into $VSX_SRC"
    mkdir -p "$HEAVY_CACHE"
    git clone --depth 1 --branch "$VSX_REF" "$VSX_REPO_URL" "$VSX_SRC"
  fi
fi
[[ -f "$VSX_SRC/tests/heavy/view.sh" ]] || fail "$VSX_SRC is not a batlehub-vsx checkout (no tests/heavy/view.sh)"
log "Extension repository: $VSX_SRC ($(git -C "$VSX_SRC" log --oneline -1 2>/dev/null || echo 'not a git checkout'))"

# Its dependencies: puppeteer-core for the driver, esbuild and vsce to
# package. The repository pins pnpm through mise; use that toolchain when it
# is there, the caller's pnpm otherwise.
if [[ ! -d "$VSX_SRC/extensions/batlehub-vsx/node_modules/puppeteer-core" ]]; then
  log "Installing the extension repository's dependencies"
  (
    cd "$VSX_SRC"
    if command -v mise >/dev/null 2>&1 && [[ -f mise.toml ]]; then
      mise install --quiet >/dev/null 2>&1 || true
      eval "$(mise env -s bash 2>/dev/null)" || true
    fi
    command -v pnpm >/dev/null 2>&1 || fail "pnpm is not on PATH and mise did not provide it"
    pnpm install --frozen-lockfile
  )
fi

# ── 1. Its suite, against this checkout ─────────────────────────────────────

export BATLEHUB_SRC="$ROOT"
export HEAVY_CACHE CDP_URL
export VSCODE_VERSION="${VSCODE_VERSION:-1.136.2}"
log "Running $VSX_SRC/tests/heavy/view.sh with BATLEHUB_SRC=$ROOT (VS Code $VSCODE_VERSION, browser $CDP_URL)"
(
  cd "$VSX_SRC"
  if command -v mise >/dev/null 2>&1 && [[ -f mise.toml ]]; then
    eval "$(mise env -s bash 2>/dev/null)" || true
  fi
  bash tests/heavy/view.sh
) || { echo "screenshots and logs: $VSX_SRC/tests/heavy/work/last" >&2; fail "the extension's suite failed against this checkout"; }
log "VSX-EXTENSION-HEAVY-OK (batlehub-vsx, both modes, in VS Code $VSCODE_VERSION against this checkout — screenshots in $VSX_SRC/tests/heavy/work/last/shots)"
