# Rust toolchain (rustup)

Proxy and cache the `static.rust-lang.org` distribution as a *typed* registry, so a toolchain can be **blocked** rather than merely cached. The channel manifests — `channel-rust-stable.toml`, `channel-rust-beta.toml`, `channel-rust-nightly.toml` and the dated ones under `{date}/` — are the filtered listing and the enforcement point; the per-target component tarballs and their `.sha256` siblings are the artifacts.

This is the toolchain, not the crates. `cargo` resolves dependencies against a separate [`cargo`](./cargo) registry, and a closed estate normally runs both.

## At a glance

| | |
|---|---|
| **Config type** | `rustup` |
| **Default upstream** | `static.rust-lang.org` |
| **Modes** | proxy-only |
| **Addressing** | one package per channel, one version per release, one file per target triple |
| **Private publish** | ❌ proxy-only |
| **Client switch** | `RUSTUP_DIST_SERVER` (and `RUSTUP_UPDATE_ROOT` for rustup's own updates) |

## Proxy setup

`RUSTUP_DIST_SERVER` is the one switch rustup needs. Export it before rustup runs — in `/etc/profile.d`, a `Containerfile`, or a CI job's `env:` block. Replace `<registry>` with your configured registry name:

```sh
# The one client switch. Export it before rustup runs — in /etc/profile.d,
# a Containerfile, or a CI job's env: block.
export RUSTUP_DIST_SERVER="https://batlehub.example.com/proxy/<registry>/rustup"
# Only needed if rustup should also update *itself* through the proxy.
export RUSTUP_UPDATE_ROOT="https://batlehub.example.com/proxy/<registry>/rustup/rustup"

rustup toolchain install stable --profile minimal
cargo --version
```

Your administrator's registry block:

```toml
[[registries]]
name      = "<registry>"
type      = "rustup"
mode      = "proxy"                              # the only mode: no publish protocol
upstreams = ["https://static.rust-lang.org"]     # the default

[registries.rbac]
# The manifests are listings; the component tarballs are reads. An install needs both.
anonymous = ["releases:read", "releases:list"]
user      = ["releases:read", "releases:list"]
admin     = ["*"]
```

## Authentication

rustup has **no token flag, no credential file and no `~/.netrc` support**. What it does have — measured on the wire rather than read off its documentation — is HTTP Basic from the URL's userinfo, so the credential goes in the variable itself:

```sh
# rustup exposes no token flag, reads no ~/.netrc and has no credential file.
# What it does do — measured on the wire — is send HTTP Basic from the URL's
# userinfo, so the credential goes in the variable itself.
export RUSTUP_DIST_SERVER="https://<user>:<token>@batlehub.example.com/proxy/<registry>/rustup"
export RUSTUP_UPDATE_ROOT="https://<user>:<token>@batlehub.example.com/proxy/<registry>/rustup/rustup"
```

The token travels in the **password** field; the user part is not read by the server and is there because a URL with a password and no user is not a URL.

::: warning A URL carrying a secret is visible
It lands in shell history, in `ps` output and in any log that echoes the environment. Prefer a CI secret, or a profile file with mode `0600` that nothing else reads.
:::

## What blocking does to a client

A blocked toolchain or component is **absent from the channel manifest**, so rustup stops with its own error before attempting any download — the same failure it gives for a version that upstream never published. There is no partial install to clean up.

## The signature caveat

Upstream publishes a detached PGP signature (`.asc`) beside each manifest. BatleHub relays it byte-exact and never re-signs, so a *filtered* manifest no longer matches the signature it ships with.

In practice rustup's signature verification is off by default and warns rather than fails, so a filtered manifest installs and prints a warning. An estate that turns verification on must accept either the warning or an unfiltered manifest — the two cannot both hold, and the same trade-off is recorded for the air-gap listings of RFC 0008-bis.

## See also

- [`cargo`](./cargo) — the crates, which are a separate registry
- [RFC 0024](../rfc/0024-rustup-dist) — why the tree is a typed adapter rather than a `generic` mirror
