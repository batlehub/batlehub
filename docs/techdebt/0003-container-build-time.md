# TD-0003 — The release container build rebuilds what it does not need to

| | |
| --- | --- |
| **Status** | Open |
| **Found** | 2026-10-14, watching the `v1.6.0` Release run (`38622313628`) |
| **Area** | `Containerfile`, `Containerfile.worker`, `Containerfile.hardened`, `.github/workflows/build.yaml` |

The `Build and push container image` job is what a release waits for. For
`v1.5.0` (run `38465664268`) its `Build and push image` step alone took
**3 h 04** (23:38 → 02:42), and the whole run 3 h 31. For `v1.6.0` the same step
was still running after 3 h. Every other job of the run was done in under
20 minutes, and `Build and publish server binary` built the same server in
19 minutes. Nobody has measured how those 3 hours split up. The causes below
come from reading the files. They are the likely ones, not proven yet.

## 1. arm64 is compiled under QEMU

`build.yaml` builds `platforms: linux/amd64,linux/arm64` on an amd64 runner,
with `docker/setup-qemu-action`. So the arm64 `cargo build --release` of the
whole dependency tree runs emulated, which is typically an order of magnitude
slower than native.

**Retired when** arm64 is built natively (an arm64 runner per platform, joined
into a manifest list) or cross-compiled on the host (`--platform=$BUILDPLATFORM`
in the builder stage + a target triple), and the step's time is measured
before and after.

## 2. The dependency layer is invalidated by every release

The `Containerfile` dependency-cache trick copies `Cargo.toml` and `Cargo.lock`
and then builds stubs. But a release bumps `[workspace.package] version` in the
root `Cargo.toml`, and the version of every workspace crate in `Cargo.lock`.
So on a release, which is the only build that matters here, the cached layer
never matches, and every dependency recompiles, twice over under QEMU (§1).

The stub build also ends in `2>/dev/null; exit 0`, so when it fails, nothing
shows it and the cache is silently empty. `Containerfile.worker` and
`Containerfile.hardened` copy the same pattern.

**Retired when** the dependency layer stops keying on the workspace version
(e.g. `cargo-chef`, which normalises it, or a BuildKit `--mount=type=cache` on
`target/` and `~/.cargo/registry`), and a stub-build failure fails the build.

## 3. The same server is compiled three times per release

`Build and publish server binary` builds `batlehub-server`. Then the
`Containerfile` builder builds it again (amd64 + arm64), and
`Containerfile.worker` builds it a third time from its own builder stage
(amd64). None of them reuses another's output.

**Retired when** the images copy a binary built once per target (a shared
builder stage or a build artefact passed in), not one they compile themselves.

## 4. The UI stage waits for the whole Rust build

`ui-builder` copies `/build/target/release/batlehub` from the builder, just to
run `dump-spec` and regenerate the TypeScript client. So `pnpm install` and
`vite build` cannot start until Rust is done, and any Rust change rebuilds the
UI. `ui/openapi.json` is already committed, and `ui/src/client/` is generated
from it.

**Retired when** the UI stage builds from the committed spec, with no
dependency on the builder, and the drift between the spec and the server is
checked in CI instead (it may already be, check before adding a gate).

## 5. Two images share one GitHub Actions cache scope

`Build and push image` and `Build and push worker image` both use
`cache-from: type=gha` / `cache-to: type=gha,mode=max` without a `scope`. They
therefore write to the same default scope, and the second can evict the first's
layers before the next run reads them.

**Retired when** each `docker/build-push-action` step sets its own
`scope=<image>` on both `cache-from` and `cache-to`.
