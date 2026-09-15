#!/usr/bin/env bash
# The install directories *this repository's* mise.toml asks for, comma
# separated — the `--only-tools` argument of `mise_cve_report.py`.
#
# `MISE_GLOBAL_CONFIG_FILE=/dev/null` is the whole point. mise merges
# `~/.config/mise/config.toml` into every resolution and keeps a tool installed
# for as long as any tracked config asks for it, so a workstation's install
# tree holds its owner's global tools and every other project's as well: this
# one measured 416 fixable HIGH/CRITICAL on 2026-09-15, of which 255 belonged
# to `~/.config/mise` (helm-ls, k9s, kkrew-tree), to `/projects/operator`
# (etcd, kube-apiserver) and to this repo's own `examples/terraform`. A CI
# runner installs this file and nothing else, so narrowing the local scan to
# the same set is what makes the two numbers comparable — and comparable is
# the only thing a budget is for.
#
# Prints one line. Names are install *directories*, not `mise.toml` keys:
# `cargo:cargo-audit` installs into `cargo-cargo-audit`, which is what Trivy
# reports a finding against.
set -euo pipefail

MISE_GLOBAL_CONFIG_FILE=/dev/null mise ls --current --json | python3 -c '
import json, os, sys

tools = json.load(sys.stdin)
print(
    ",".join(
        sorted(
            {
                os.path.basename(os.path.dirname(entry["install_path"]))
                for versions in tools.values()
                for entry in versions
                if entry.get("install_path")
            }
        )
    )
)
'
