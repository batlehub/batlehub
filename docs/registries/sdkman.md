# SDKMAN

Proxy and cache SDKMAN — the JDK, Gradle, Maven, Kotlin and every other candidate — as one registry: the candidates API (`api.sdkman.io/2`) and the download broker (`broker.sdkman.io`) behind one `type = "sdkman"` block. `sdk list` and `sdk install` resolve through listings with blocked versions removed, a blocked version answers `invalid` where `sdk install` validates it, and the broker's redirect to the vendor's CDN is followed **server-side**, so the 200 MB JDK is cached here rather than referred elsewhere.

## At a glance

| | |
|---|---|
| **Config type** | `sdkman` |
| **Default upstream** | `api.sdkman.io/2` (candidates API) · `broker.sdkman.io` (`broker_url`) |
| **Modes** | proxy-only |
| **Addressing** | `{candidate}/{version}/{platform}` — one archive per platform |
| **Private publish** | ❌ proxy-only |
| **Air gap** | offline, `versions/all` and `candidates/default` are composed from the held candidate archives for the platform |

## Proxy setup

`sdkman-init.sh` sets its two API variables only when they are empty, so export both **before** sourcing it — in `/etc/profile.d`, a `Containerfile`, or a CI job's `env:` block. Replace `<registry>` with your configured registry name:

```sh
export SDKMAN_CANDIDATES_API="https://batlehub.example.com/proxy/<registry>/sdkman"
export SDKMAN_BROKER_API="https://batlehub.example.com/proxy/<registry>/sdkman/broker"
source "$HOME/.sdkman/bin/sdkman-init.sh"

sdk list java                # the rendered table, blocked versions removed
sdk install java 21.0.5-tem  # validate, download through the broker route, hook
```

Your administrator's registry block:

```toml
[[registries]]
name       = "jvm"
type       = "sdkman"
mode       = "proxy"                        # the only mode: no publish protocol
upstreams  = ["https://api.sdkman.io/2"]    # the candidates API (the /2 is part of it)
broker_url = "https://broker.sdkman.io"     # the download broker

[registries.rbac]
# candidates, versions, validate, hooks and healthcheck are listings
# (`releases:list`); the download is a read (`releases:read`). An install needs both.
anonymous = ["releases:read", "releases:list"]
```

`broker_url` is SDKMAN's second host and nobody else's: it is rejected on any other registry type. The `/2` in `upstreams` is served as given — an operator pointing at `https://beta.sdkman.io/2` must be able to say so — and a URL without it is warned about at load, because it is more likely a typo than a choice.

**What the installer is not.** BatleHub proxies the registry, not `get.sdkman.io`: bootstrapping SDKMAN itself is one `curl | bash` against the installer's own host, documented as an air-gap prerequisite rather than mirrored ([RFC 0010](/rfc/0010-toolchain-managers) decision 9). The `broker/version/sdkman/…` and `selfupdate/…` endpoints *are* served, because the running client calls them.

## Blocked versions

A block is on the **candidate**: blocking `java 21.0.5-tem` covers all eight platforms, not the one whose listing you were reading. It reaches every place `sdk` looks:

- `versions/all` — the version is removed from the comma-separated list.
- `candidates/default/{c}` — if the default names the blocked version, it is repaired to the newest allowed one, the way npm's `dist-tags.latest` is.
- the rendered `versions/list` — the table `sdk list <candidate>` prints, in both of its layouts. In the Java vendor table the whole row goes and the vendor name is promoted to the next row of its block; in the grid every other candidate uses, the cell is blanked to its own width and nothing moves. Nothing is re-rendered, so an upstream layout change degrades to a version we failed to hide, never to a corrupted table.
- `candidates/validate/{c}/{v}/{plat}` — **the chokepoint**. Every `sdk install` validates here and stops on anything but `valid`; a blocked version answers `invalid` without upstream being asked, and SDKMAN prints its own refusal (*"Stop! java 21.0.5-tem is not available … is an invalid version"*). No download is attempted.

A direct request to `broker/download/…` for a blocked version still gets the operator's `403` and reason: hiding governs resolution, it does not replace diagnosis.

See [blocking a package version](/guide/admin-policies#block-a-package-version) and [which listings are filtered](/guide/admin-policies#which-listings-are-filtered).

## Authentication

`sdk` builds its own `curl` command and has nowhere to put a header. libcurl reads `~/.netrc` without being asked, so an authenticated instance needs one entry for the proxy host:

```text
machine batlehub.example.com
login <your-user-id>
password <your-token>
```

## Notes

- **The hook scripts are relayed byte-exact.** `hooks/pre` and `hooks/post` return bash that `sdk` sources and runs as the invoking user — the Linux JDK hook repackages the tarball with `tar` and `zip`. That is SDKMAN's design and what the client does today from `api.sdkman.io` directly; BatleHub neither adds nor removes trust by carrying the bytes, and does not modify them, because rewriting a URL inside a hook would make it a co-author of executed shell.
- **The redirect chain is followed through the SSRF guard.** The broker names the download host, and that host is not SDKMAN: `github.com`, `repo.maven.apache.org`, `services.gradle.org`, and whatever a vendor publishes on. Every hop is checked against the guard and the operator's upstream credentials stop at the two configured origins. Egress to those CDN hosts is a prerequisite; the list is observed, not exhaustive — see [What leaves this instance](/operations/egress).
- **No dates, so an age gate must state its posture.** SDKMAN publishes no publish dates, so every artifact reaches a `release_age_gate` with `published_at = None`. On this kind `deny_missing_timestamp` is **mandatory**: `true` refuses every download on the registry, `false` makes the gate inert, and neither is a default this server picks for you. A `[registries.security]` block ([RFC 0018](/rfc/0018-supply-chain-quarantine-and-verdicts)) holds on a missing timestamp by default instead.
- **`X-Sdkman-*` response headers are not forwarded.** The client reads `X-Sdkman-Checksum-<ALG>` to verify a download; no candidate sampled emits one, and the client's own reader (`grep '^X-Sdkman'`) matches nothing over the lower-cased header names the API sends today, so the check is inert with or without a proxy. Recorded in RFC 0010 §13.2 rather than left to be discovered.
- **Cache warming needs a platform.** `warm_packages = ["java@21.0.5-tem"]` warms one archive per entry of `cache.warm_platforms`, defaulting to the platform this server runs on; guessing all eight would fetch 1.6 GB of JDK for a one-line `.sdkmanrc`. `batlehub-cli registry suggest` reads `.sdkmanrc` and writes both.
- **The rendered list is cached per client.** Its cache key includes the `?current=&installed=` query `sdk list` sends, so two machines with different installed sets never share an entry.

## Endpoints

<!-- BEGIN endpoints: proxy/sdkman -->
| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/proxy/{registry}/sdkman/broker/download/{candidate}/{version}/{platform}` | The archive for one version on one platform, streamed through the |
| `GET` | `/proxy/{registry}/sdkman/broker/version/sdkman/{component}/{channel}` | The current SDKMAN script or native version on a channel — what `sdk |
| `GET` | `/proxy/{registry}/sdkman/candidates/{candidate}/{platform}/versions/all` | Every version of a candidate on a platform, comma-separated, blocked |
| `GET` | `/proxy/{registry}/sdkman/candidates/{candidate}/{platform}/versions/list` | The rendered table `sdk list <candidate>` prints, in either of its two |
| `GET` | `/proxy/{registry}/sdkman/candidates/all` | Every candidate name, comma-separated — what `sdk update` caches and |
| `GET` | `/proxy/{registry}/sdkman/candidates/default/{candidate}` | The version `sdk install <candidate>` resolves to with no version given, |
| `GET` | `/proxy/{registry}/sdkman/candidates/list` | The rendered candidate table `sdk list` prints with no argument. |
| `GET` | `/proxy/{registry}/sdkman/candidates/validate/{candidate}/{version}/{platform}` | The chokepoint: `valid` or `invalid` for one version on one platform. A |
| `GET` | `/proxy/{registry}/sdkman/healthcheck` | The API's health token, read by every `sdk` invocation. Relayed as-is: |
| `GET` | `/proxy/{registry}/sdkman/hooks/{phase}/{candidate}/{version}/{platform}` | A pre- or post-install hook: bash the client sources and runs, relayed |
| `GET` | `/proxy/{registry}/sdkman/selfupdate/{channel}/{platform}` | The self-update script for a channel and platform — bash the client |
<!-- END endpoints -->

## See also

- [Node distributions](/registries/nodedist) — the other toolchain kind of RFC 0010
- [Using BatleHub](/use/) — tokens, publishing prerequisites, the CLI
- [Registries overview](/registries/) · [Caching](/guide/caching) · [Access Control](/guide/access-control)
