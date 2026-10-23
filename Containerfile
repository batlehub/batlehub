# ── Build stage ───────────────────────────────────────────────────────────────
FROM rust:1.98-slim-bookworm@sha256:ebd900bae66fd508b466cef82d64a83a5fb34682e4c8b2797a42908bddc95a57 AS chef

RUN apt-get update && apt-get install -y --no-install-recommends \
    curl libssl-dev make pkg-config \
    && rm -rf /var/lib/apt/lists/*
RUN cargo install --locked cargo-chef --version 0.1.78

WORKDIR /build

# cargo-chef rather than stub `lib.rs` files: every `chore(version)` rewrites
# the workspace version in `Cargo.toml` and `Cargo.lock`, so a layer keyed on
# those files was cold on every release and rebuilt the whole dependency graph.
# The recipe masks workspace-member versions (all `0.0.1`), so it only changes
# when a dependency does.
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates/  crates/
COPY server/  server/
COPY cli/     cli/
COPY patches/ patches/
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
# `patches/` is not a workspace member, so it is not in the recipe and has to be
# real before the cook: `tikv-jemalloc-ctl` compiles against the `paste` shim.
COPY patches/ patches/
COPY --from=planner /build/recipe.json recipe.json
# --locked: build exactly what Cargo.lock resolves and fail if it would need
# updating — the image must not resolve a dependency the repository never saw.
# Same package selection as the build below, or feature unification differs and
# the shared dependencies are compiled twice.
RUN cargo chef cook --locked --release --recipe-path recipe.json -p batlehub-server

COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY server/ server/
COPY cli/    cli/
RUN cargo build --locked --release -p batlehub-server

# Pre-create runtime directories so they can be copied into the shell-less distroless image.
RUN mkdir -p /var/cache/batlehub

# ── Server ────────────────────────────────────────────────────────────────────
# The binary alone. The release exports this stage once on amd64 and hands it to
# the release page and to `Containerfile.worker`'s `server` build context, so the
# server is compiled once per arch, not once per artefact (TD-0003 §3).
FROM scratch AS server
COPY --from=builder /build/target/release/batlehub /batlehub

# ── CLI ───────────────────────────────────────────────────────────────────────
# The release overrides this stage with `--build-context cli=<dir>` holding the
# static musl binary its `cli` job already built and attested, so the image ships
# the same bytes as the release page and never compiles the CLI. A local build
# or `image-scan.yaml` has no such directory and builds it here.
FROM builder AS cli-build
RUN cargo build --locked --release -p batlehub-cli

FROM scratch AS cli
COPY --from=cli-build /build/target/release/batlehub-cli /batlehub-cli

# ── Frontend build stage ───────────────────────────────────────────────────────
FROM node:26-slim@sha256:14bf3eac4bf209d906d3c41256597d3ab1f926b2e93a79e9bdfe1efd32454239 AS ui-builder

WORKDIR /ui
# Corepack is no longer distributed with Node (removed in Node 25), so pnpm is
# installed explicitly. Keep this version in sync with the `packageManager`
# field in ui/package.json. pnpm ships no install-time lifecycle scripts, so
# --ignore-scripts changes nothing today; it states the policy so a future
# release that adds one cannot run it here.
RUN npm install -g --ignore-scripts pnpm@11.26.0
COPY ui/package.json ui/pnpm-lock.yaml ui/pnpm-workspace.yaml ./
# --frozen-lockfile is the `npm ci` equivalent: it fails rather than silently
# resolving something the committed lockfile does not describe.
RUN pnpm install --frozen-lockfile

COPY ui/ ./

# The client is generated from the committed spec, not from a `dump-spec` of the
# builder's binary, so this stage runs alongside the Rust build instead of after
# it (TD-0003 §4). `openapi_spec_is_committed` in `crates/web/tests` fails when
# the committed spec and the server disagree.
RUN pnpm run generate && pnpm run build

# ── Runtime image ─────────────────────────────────────────────────────────────
FROM gcr.io/distroless/cc-debian12:latest@sha256:e5d81ddde149641e2a9ba55be4545bc125c67de07508b03ba4c22e6eb0ded5aa AS runtime

COPY --from=server   /batlehub                          /usr/local/bin/batlehub
COPY --from=cli      /batlehub-cli                      /usr/local/bin/batlehub-cli
# The cache directory is the one path the process writes to when
# `[storage] type = "filesystem"`, so it must be owned by the runtime user.
# Everything else (the binaries, the SPA bundle) stays root-owned and
# read-only to that user, which is what we want.
COPY --from=builder --chown=65532:65532 /var/cache/batlehub /var/cache/batlehub
COPY --from=ui-builder /ui/dist                         /app/ui/dist

EXPOSE 8080

# 65532 is distroless's `nonroot` user. Declared numerically because the image
# ships no shell and no /etc/passwd lookup is guaranteed at runtime; Kubernetes
# also needs a numeric UID to satisfy `runAsNonRoot` without resolving names.
# The chart pins the same UID in its podSecurityContext — keep the two in sync.
USER 65532:65532

ENTRYPOINT ["batlehub"]
CMD ["--config", "/etc/batlehub/config.toml"]
