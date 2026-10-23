# TD-0003 — The release container build rebuilds what it does not need to

| | |
| --- | --- |
| **Status** | Retired |
| **Found** | 2026-10-14, watching the `v1.6.0` Release run (`38622313628`) |
| **Area** | `Containerfile`, `Containerfile.worker`, `Containerfile.hardened`, `.github/workflows/build.yaml` |

The `Build and push container image` job is what a release waits for. For
`v1.5.0` (run `38465664268`) its `Build and push image` step alone took
**3 h 04** (23:38 → 02:42), and the whole run 3 h 31. For `v1.6.0` the same step
was still running after 3 h. Every other job of the run was done in under
20 minutes, and `Build and publish server binary` built the same server in
19 minutes. Nobody has measured how those 3 hours split up. The causes below
come from reading the files. They are the likely ones, not proven yet.

`v1.8.1` (run of 2026-10-21) put a number on §1: the job took 4 h, of which
3 h 28 for the multi-arch proxy image and 27 min for the amd64-only worker
image, so roughly 3 h of it was the arm64 leg under QEMU.

## 1. arm64 is compiled under QEMU

`build.yaml` builds `platforms: linux/amd64,linux/arm64` on an amd64 runner,
with `docker/setup-qemu-action`. So the arm64 `cargo build --release` of the
whole dependency tree runs emulated, which is typically an order of magnitude
slower than native.

**Retired when** arm64 is built natively (an arm64 runner per platform, joined
into a manifest list) or cross-compiled on the host (`--platform=$BUILDPLATFORM`
in the builder stage + a target triple), and the step's time is measured
before and after.

**Retired**: `image-proxy` builds each arch on its own native runner
(`ubuntu-latest`, `ubuntu-24.04-arm`) and pushes by digest; `image` joins the
two into the tagged index with `docker buildx imagetools create`. QEMU is gone
from the workflow. Measured in [§ Measurements](#measurements): the release
went from 4 h 16 to 28 min.

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

**Retired** for `Containerfile` and `Containerfile.worker`: both cook with
`cargo-chef`, whose recipe pins every workspace-member version to `0.0.1`, and a
failed `cargo chef cook` fails the build. So does `Containerfile.hardened`,
which no workflow builds.

Checked by preparing the recipe before and after `task version:set`'s edit
(`1.8.1` → `1.8.2` in `Cargo.toml`, the seven workspace entries in
`Cargo.lock`): both recipes have the same sha256 (`27dbb786…`), so the cook
layer's cache key does not move. Changing one third-party version in the lock
does change the recipe, so the cache still invalidates when it should. Seen end to end
in `v1.8.2-rc.2` ([§ Measurements](#measurements)).

## 3. The same server is compiled three times per release

`Build and publish server binary` builds `batlehub-server`. Then the
`Containerfile` builder builds it again (amd64 + arm64), and
`Containerfile.worker` builds it a third time from its own builder stage
(amd64). None of them reuses another's output.

**Retired when** the images copy a binary built once per target (a shared
builder stage or a build artefact passed in), not one they compile themselves.

The CLI part of this is retired: the release passes the `cli` job's static
musl binaries in as the `cli` build context, so the proxy image no longer
compiles the CLI (and ships the same bytes as the release page).

**Retired** for the server too: the proxy image's amd64 leg builds its `server`
stage first and uploads the binary as `server-linux-amd64`. The `server` job
publishes and attests that file instead of compiling its own, and
`image-worker` passes it as the `server` build context of
`Containerfile.worker`, whose own builder now only runs for a local build or
`image-scan.yaml`. One compile per arch. The release binary is therefore built
on bookworm (glibc 2.36) rather than `ubuntu-latest`, so it runs on more hosts,
not fewer. The cost is ordering: `server` and `image-worker` now wait for
`image-proxy` instead of running beside it, which `v1.8.2-rc.2` shows costs nothing: the
worker build now takes a minute.

## 4. The UI stage waits for the whole Rust build

`ui-builder` copies `/build/target/release/batlehub` from the builder, just to
run `dump-spec` and regenerate the TypeScript client. So `pnpm install` and
`vite build` cannot start until Rust is done, and any Rust change rebuilds the
UI. `ui/openapi.json` is already committed, and `ui/src/client/` is generated
from it.

**Retired when** the UI stage builds from the committed spec, with no
dependency on the builder, and the drift between the spec and the server is
checked in CI instead (it may already be, check before adding a gate).

**Retired**: `ui-builder` runs `pnpm run generate` on the committed spec and no
longer copies anything from the builder. Nothing checked the drift before:
`front-test.yaml` regenerates the spec without comparing it, and only runs on
`ui/` paths. `openapi_spec_is_committed` in `crates/web/tests/openapi_contract.rs`
now fails whenever `ui/openapi.json` is not byte-for-byte what `dump-spec`
prints, so a Rust change that moves the API has to commit `task dump-spec`.
`Containerfile.hardened` has the same stage.

## 5. Two images share one GitHub Actions cache scope

`Build and push image` and `Build and push worker image` both use
`cache-from: type=gha` / `cache-to: type=gha,mode=max` without a `scope`. They
therefore write to the same default scope, and the second can evict the first's
layers before the next run reads them.

**Retired when** each `docker/build-push-action` step sets its own
`scope=<image>` on both `cache-from` and `cache-to`.

**Retired**, and not by a scope: a tag run can only read the Actions cache of
its own ref and of `main`, so a gha cache written by one release was never
readable by the next one. Each build now caches to its own registry ref,
`ghcr.io/<repo>/buildcache:<image>[-<arch>]`.

## Measurements

Release runs (`build.yaml`) before and after §1, §2 and §5, then §3 and §4:

| | `v1.8.1` (run `39657343550`) | `v1.8.2-rc.1` (run `39839300187`) | `v1.8.2-rc.2` (run `39990328020`) |
| --- | ---: | ---: | ---: |
| **Whole release run** | **4 h 16** | **28 min** | **26 min** |
| Proxy image, both arches | 3 h 28 (one job, arm64 under QEMU) | — | — |
| Proxy image, amd64 | — | 24 min (native) | 17 min 29 |
| Proxy image, arm64 | — | 23 min (native) | 17 min 06 |
| Server binary | compiled on its own | compiled on its own | 1 min 26 (the image's, attested) |
| Worker images | 27 min, after the proxy image | 22 min, alongside it | 1 min 03, after the proxy image |
| Publish, sign, scan (index + 3 images) | inside the 4 h job | 3 min 30 | 3 min 37 |
| CLI Linux aarch64 | 5 min (`cross`) | 4 min 25 (native) | 4 min 49 |

Both earlier release runs were cold: `v1.8.1` because of §2 and §5, `v1.8.2-rc.1`
because it created the registry cache. The `cli` jobs were done by 19:26 and
the proxy legs by 19:45, so waiting for the CLI artefact cost nothing.

`v1.8.2-rc.2` is the first release to read a warm `buildcache:proxy-<arch>`:
on both arches `cargo chef cook` came back `CACHED`, and what is left of a
proxy leg is the workspace's own `cargo build` (902 s on amd64, 886 s on arm64).
The worker build passed `--build-context server=dist/server` and compiled
nothing. Its `Checksums and detached signature` job failed once, on an
`HttpError` from `softprops/action-gh-release` while the perf workflow was writing
to the same draft, and passed on a rerun; the 26 min exclude the rerun.

The image of `v1.8.2-rc.1` carries the `cli` job's binary: extracted from both
published images, `batlehub-cli` is a static ELF of the right arch and
byte-identical to `batlehub-cli-linux-<arch>.tar.gz` on the release.

Warm, measured with the `image` label on PR #226 (`pr-image.yaml`, same
Containerfile, `buildcache:pr-<arch>`):

| | amd64 | arm64 |
| --- | ---: | ---: |
| Cold (run `39773342781`) | 28 min 43 | 26 min 44 |
| Every layer cached (run `39803105238`) | 6 s | 5 s |
