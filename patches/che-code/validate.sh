#!/usr/bin/env bash
# Validate the gallery-credential patch against a real che-code tree.
#
# What "validated" means here, and what this script measures again:
#
#   1. the carried diff (`che-code-main-<sha>.diff`) applies to a pristine
#      checkout of that commit — `git apply --check`;
#   2. the two TypeScript files it touches type-check under che-code's own
#      `tsconfig.base.json` (strict, nodenext) with **no new error**: the tree
#      is checked before and after, and the two error sets must be identical
#      (a scratch install lacks `@vscode/proxy-agent` and `kerberos`, so the
#      baseline is never zero — what matters is that the patch adds nothing);
#   3. the module's own tests pass under `node --test`.
#
# Needs: git, node ≥ 22.6 (type stripping), npm, network for the clone and
# for `typescript` + `@types/node` into a scratch directory.
#
#   patches/che-code/validate.sh                 # the commit the diff names
#   CHE_CODE_REF=main patches/che-code/validate.sh   # a newer tree: expect the
#                                                    # apply step to say what moved
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DIFF="$(ls "$HERE"/che-code-main-*.diff | head -1)"
SHA="$(basename "$DIFF" .diff | sed 's/^che-code-main-//')"
REF="${CHE_CODE_REF:-$SHA}"
CACHE="${HEAVY_CACHE:-$HOME/.cache/batlehub-heavy}"
TREE="$CACHE/che-code"
SCRATCH="${TMPDIR:-/tmp}/che-code-validate"
NODE_FILE="code/src/vs/platform/request/node/requestService.ts"
BROWSER_FILE="code/src/vs/workbench/services/request/browser/requestService.ts"
log() { printf '\n==> %s\n' "$*"; }

log "che-code at $REF (diff carried for $SHA)"
if [[ ! -d "$TREE/.git" ]]; then
  git clone -q --depth 1 https://github.com/che-incubator/che-code "$TREE"
fi
git -C "$TREE" fetch -q --depth 1 origin "$REF" 2>/dev/null || git -C "$TREE" fetch -q --depth 1 origin main
git -C "$TREE" checkout -q --detach FETCH_HEAD 2>/dev/null || git -C "$TREE" checkout -q --detach "$REF"
git -C "$TREE" reset -q --hard && git -C "$TREE" clean -qfd
echo "tree: $(git -C "$TREE" log --oneline -1), VS Code $(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["version"])' "$TREE/code/package.json")"

log "1. git apply --check"
git -C "$TREE" apply --check "$DIFF" && echo "applies cleanly"

log "2. type-check before and after, under code/src/tsconfig.base.json"
mkdir -p "$SCRATCH"
if [[ ! -x "$SCRATCH/node_modules/.bin/tsc" ]]; then
  (cd "$SCRATCH" && printf '{ "name": "che-code-validate", "private": true }\n' > package.json \
    && npm install --no-audit --no-fund --silent typescript@5 @types/node@24)
fi
tsconfig() {  # <file> → a tsconfig checking that one file
  cat > "$SCRATCH/tsconfig.$2.json" <<JSON
{
  "extends": "$TREE/code/src/tsconfig.base.json",
  "compilerOptions": { "noEmit": true, "skipLibCheck": true, "esModuleInterop": true, "resolveJsonModule": true, "allowJs": true, "isolatedModules": false, "types": ["node"], "typeRoots": ["$SCRATCH/node_modules/@types"] },
  "files": ["$TREE/$1"],
  "include": ["$TREE/code/src/typings/*.d.ts"]
}
JSON
}
tsconfig "$NODE_FILE" node; tsconfig "$BROWSER_FILE" browser
errors() { "$SCRATCH/node_modules/.bin/tsc" -p "$SCRATCH/tsconfig.$1.json" 2>&1 | /bin/grep "error TS" | sed 's/([0-9]*,[0-9]*)//' | sort || true; }
errors node > "$SCRATCH/node.before"; errors browser > "$SCRATCH/browser.before"
git -C "$TREE" apply "$DIFF"
errors node > "$SCRATCH/node.after"; errors browser > "$SCRATCH/browser.after"
for f in node browser; do
  if diff -q "$SCRATCH/$f.before" "$SCRATCH/$f.after" >/dev/null; then
    echo "$f: $(wc -l < "$SCRATCH/$f.after") pre-existing error(s), none added"
  else
    echo "$f: the patch changed the error set:"; diff "$SCRATCH/$f.before" "$SCRATCH/$f.after" | /bin/grep '^[<>]' || true; exit 1
  fi
done

log "3. the module's tests, in place"
cp "$HERE/vsxRegistryAuth.test.ts" "$TREE/code/src/vs/platform/request/node/"
TAP="$(cd "$TREE/code/src/vs/platform/request/node" && node --test --test-reporter=tap vsxRegistryAuth.test.ts 2>&1)" || true
rm -f "$TREE/code/src/vs/platform/request/node/vsxRegistryAuth.test.ts"
echo "$TAP" | /bin/grep -E "^# (pass|fail) " || { echo "$TAP" | tail -20; echo "no TAP summary — node --test did not run"; exit 1; }
echo "$TAP" | /bin/grep -q "^# fail 0$" || { echo "$TAP" | /bin/grep -B2 -A8 "^not ok" | head -40; exit 1; }
git -C "$TREE" reset -q --hard && git -C "$TREE" clean -qfd
log "VALIDATED against che-code $(git -C "$TREE" rev-parse --short HEAD)"
