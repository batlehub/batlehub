# Registries

BatleHub proxies, caches, and privately hosts **23 registry types** — from language package managers to OS package repositories, editor extension marketplaces, and generic file mirrors.

Every registry type can run in one of three **modes**, set per registry in the config:

- **proxy** — a pure read-through cache in front of an upstream. The first request is fetched from upstream and stored; every later request is served from cache.
- **local** — a fully private registry. Nothing is fetched from upstream; you publish and serve your own artifacts.
- **hybrid** — local artifacts win, and anything not published locally falls through to the upstream proxy.

Five types are **proxy-only** (no private publish model): **GitHub**, **Forgejo**, **GitLab** (they host source/releases upstream) and **JetBrains** IDE archives + **Generic** file mirrors (path-only caches).

## The registries

### Source hosting

| Registry | `type` | What it proxies | Modes | Publish | Default upstream |
|----------|--------|-----------------|-------|:-------:|------------------|
| [GitHub](./github) | `github` | Releases, assets, tarballs, raw files | proxy-only | ❌ | `api.github.com` |
| [Forgejo / Gitea](./forgejo) | `forgejo` | Releases, assets, archives, raw (`/api/v1`) | proxy-only | ❌ | `codeberg.org` |
| [GitLab](./gitlab) | `gitlab` | Releases, link assets, archives (`/api/v4`) | proxy-only | ❌ | `gitlab.com` |

### Language package managers

| Registry | `type` | What it proxies | Modes | Publish | Default upstream |
|----------|--------|-----------------|-------|:-------:|------------------|
| [npm](./npm) | `npm` | Packument + tarballs | proxy · local · hybrid | ✅ | `registry.npmjs.org` |
| [Cargo](./cargo) | `cargo` | Sparse index + `.crate` | proxy · local · hybrid | ✅ | `crates.io` |
| [Go Modules](./goproxy) | `goproxy` | GOPROXY (`.info`/`.mod`/`.zip`) | proxy · local · hybrid | ✅ | `proxy.golang.org` |
| [Maven](./maven) | `maven` | Metadata XML + JAR/POM | proxy · local · hybrid | ✅ | `repo1.maven.org` |
| [PyPI](./pypi) | `pypi` | Simple API (PEP 503/691) + wheels | proxy · local · hybrid | ✅ | `pypi.org` |
| [Conda](./conda) | `conda` | `repodata.json` + `.conda`/`.tar.bz2` | proxy · local · hybrid | ✅ | `conda.anaconda.org` |
| [Composer (PHP)](./composer) | `composer` | Packagist v2 (p2 metadata + dist) | proxy · local · hybrid | ✅ | `repo.packagist.org` |
| [RubyGems](./rubygems) | `rubygems` | Gems + versions + info API | proxy · local · hybrid | ✅ | `rubygems.org` |
| [NuGet (.NET)](./nuget) | `nuget` | v3 index + flat + `.nupkg` | proxy · local · hybrid | ✅ | `api.nuget.org` |
| [Terraform](./terraform) | `terraform` | Providers + modules (v1 API) | proxy · local · hybrid | ✅ | `registry.terraform.io` |

### Editor extensions

| Registry | `type` | What it proxies | Modes | Publish | Default upstream |
|----------|--------|-----------------|-------|:-------:|------------------|
| [OpenVSX](./openvsx) | `openvsx` | Extension VSIX | proxy · local · hybrid | ✅ | `open-vsx.org` |
| [VS Code Marketplace](./vscode-marketplace) | `vscode-marketplace` | Extension VSIX (MS Gallery) | proxy · local · hybrid | ✅ | `marketplace.visualstudio.com` |
| [JetBrains Marketplace](./jetbrains-marketplace) | `jetbrains-marketplace` | Plugin API + downloads | proxy · local · hybrid | ✅ | `plugins.jetbrains.com` |

### OS / system packages <Badge type="tip" text="path-addressed" />

| Registry | `type` | What it proxies | Modes | Publish | Default upstream |
|----------|--------|-----------------|-------|:-------:|------------------|
| [Debian / APT](./deb) | `deb` | `Packages`/`Release` + `.deb` | proxy · local · hybrid | ✅ | none — set `upstreams` |
| [RPM / YUM / DNF](./rpm) | `rpm` | `repodata/` + `.rpm` | proxy · local · hybrid | ✅ | none — set `upstreams` |
| [Pacman / Arch](./pacman) | `pacman` | `<repo>.db` + `.pkg.tar.zst` | proxy · local · hybrid | ✅ | none — set `upstreams` |

### Binaries & mirrors <Badge type="tip" text="path-addressed" />

| Registry | `type` | What it proxies | Modes | Publish | Default upstream |
|----------|--------|-----------------|-------|:-------:|------------------|
| [JetBrains IDEs](./jetbrains) | `jetbrains` | IDE installer archives | proxy-only | ❌ | `download.jetbrains.com` |
| [Generic mirror](./generic) | `generic` | Any HTTP file tree | proxy-only | ❌ | none — set `upstreams` + `path_allow` |

### Toolchains <Badge type="tip" text="RFC 0010" />

Typed, so a release can be *blocked* rather than merely cached — the identity the generic mirror cannot give the same bytes.

| Registry | `type` | What it proxies | Modes | Publish | Default upstream |
|----------|--------|-----------------|-------|:-------:|------------------|
| [Node distributions](./nodedist) | `nodedist` | `index.tab`/`index.json` + release tarballs, `SHASUMS256.txt` byte-exact (nvm, fnm, n, mise) | proxy-only | ❌ | `nodejs.org/dist` |
| [SDKMAN](./sdkman) | `sdkman` | Candidates API + download broker (the JDK, Gradle, Maven, Kotlin, …); the broker's 302 followed server-side | proxy-only | ❌ | `api.sdkman.io/2` + `broker.sdkman.io` |

## Feature matrix

Every registry and how its capabilities map across BatleHub's features. The
**path-addressed** types (Deb, RPM, Pacman, JetBrains IDEs, Generic) have no
per-package version model, so the structural axes (version listing, source
archive, binary, age gate, warming, search) show `—`. They still get
registry-level RBAC and multi-upstream fanout, and Deb/RPM/Pacman support signed
private hosting. **Forgejo** and **GitLab** mirror **GitHub**'s behaviour.

Legend: **Ver.** version listing · **Src** source archive · **Bin** binary/extension asset · **Pub** private publish · **Fan** multi-upstream fanout · **Age** release age gate · **Warm** cache warming (version enumeration) · **Search** Package Explorer upstream search. ✓ supported · `—` not applicable · ⚠ partial.

| Registry | Ver. | Src | Bin | Pub | Fan | Age | RBAC | Warm | Search |
|----------|:----:|:---:|:---:|:---:|:---:|:---:|:----:|:----:|:------:|
| GitHub | ✓ | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | — |
| Forgejo / Gitea | ✓ | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | — |
| GitLab | ✓ | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | — |
| npm | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Cargo | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Go Modules | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Maven | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | — | ✓ |
| PyPI | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Conda | ✓ ¹ | ✓ | ✓ | ✓ | ✓ | ⚠ ² | ✓ | ✓ ¹ | — |
| Composer | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | — | ✓ |
| RubyGems | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | — | ✓ |
| NuGet | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Terraform | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | — | ✓ |
| OpenVSX | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| VS Code Marketplace | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | — |
| JetBrains Marketplace | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | — | ✓ |
| Debian / APT ³ | — | — | — | ✓ | ✓ | — | ✓ | — | — |
| RPM / YUM / DNF ³ | — | — | — | ✓ | ✓ | — | ✓ | — | — |
| Pacman / Arch ³ | — | — | — | ✓ | ✓ | — | ✓ | — | — |
| JetBrains IDEs ³ | — | — | — | — | ✓ | — | ✓ | — | — |
| Generic ³ | — | — | — | — | ✓ | — | ✓ | — | — |
| Node distributions | ✓ | ✓ | ✓ | — | ✓ | ✓ ⁴ | ✓ | ✓ ⁵ | — |
| SDKMAN | ✓ | — | ✓ | — | ✓ | ⚠ ⁴ | ✓ | ✓ ⁵ | — |

> ¹ Conda has no dedicated per-package version listing API. BatleHub synthesises one by scanning `repodata.json` across `noarch`, `linux-64`, `osx-64`, `osx-arm64`, and `win-64`; results are the union of versions found on all available platforms.
>
> ² Conda timestamps come from the `timestamp` field in `repodata.json` (ms since epoch). Most packages carry it; packages without one skip the gate unless you set `deny_missing_timestamp = true` on the rule.
>
> ³ **Path-addressed** type: artifacts are fetched by file path with no per-package version model, so the structural axes show `—`. These types don't enumerate versions but can pre-warm specific files via `cache.warm_paths`, and are gated with a mandatory `path_allow` allowlist. Deb/RPM/Pacman additionally support signed private hosting (`local`/`hybrid`); JetBrains IDE archives and Generic are proxy-only.
>
> ⁴ **Toolchain age gates** (RFC 0010 §6.7): `nodedist` reads the release date from `index.tab`, so current releases are gated and a de-listed one reaches the gate undated; `sdkman` publishes no dates at all, so the gate is decided entirely by `deny_missing_timestamp`. On both kinds that field is **mandatory** on a `release_age_gate` rule.
>
> ⁵ **Warming by platform**: a Node release and an SDKMAN version are one archive *per platform*, so `warm_packages` warms the platforms in `cache.warm_platforms`, defaulting to the server's own. The console's per-version fetch button is refused for the same reason.
>
> Package Explorer upstream ("Not Yet Proxied") search: Go uses pkg.go.dev; PyPI is exact-name lookup; Terraform combines module search with namespace/exact provider lookup. The release proxies (GitHub/Forgejo/GitLab), VS Code Marketplace, Conda, and the path-addressed types have no upstream search API — see the [Package Explorer guide](/use/package-explorer-search#upstream-search).

## READMEs

What a package says about itself, per version. Whether a registry has one at all
is a property of its *protocol*, not a preference: the text either travels in a
document the proxy already fetches to resolve a version, or it sits inside the
artifact — and that decides whether the package page can show it for a version
this instance holds no bytes for.

**Held nowhere here** is the column worth reading before you go looking for a
gap: **versions + README** means the package page answers in full for a package
nothing here has ever pulled; **versions only** means it lists the versions and
says the README arrives when one is first downloaded; **neither** means the page
answers from what this instance holds and nothing else.

**Fetchable** is whether the page offers a *Fetch this version* button on those
upstream-only rows. `no` is not a limitation of the button but of the
coordinate: a Maven version is a set of files, a Terraform provider is addressed
by OS and architecture as well as version, a PyPI version is an sdist plus one
wheel per interpreter and platform, and a conda artifact carries a channel
platform and a build string — so "fetch this version" has no single meaning for
any of them. The page says which rather than showing a disabled button — see
[fetching from the console](/guide/admin-config#console-fetch).

<!-- BEGIN readme-coverage: generated by `task docs:readme-coverage`. Do not edit by hand. -->
| Registry | README source | Per version | Held nowhere here | Fetchable |
| --- | --- | --- | --- | --- |
| github | the README is one of the repository files this proxy already serves by path, under `raw/{ref}/`, so a second URL for it would be a second answer to a solved question | — | neither | no |
| forgejo | the README is one of the repository files this proxy already serves by path, under `raw/{ref}/`, so a second URL for it would be a second answer to a solved question | — | neither | no |
| gitlab | the README is one of the repository files this proxy already serves by path, under `raw/{ref}/`, so a second URL for it would be a second answer to a solved question | — | neither | no |
| cargo | a file inside the artifact | yes | versions only | yes |
| npm | the metadata document, else the artifact | yes | versions + README | yes |
| openvsx | a URL in the metadata, read separately | yes | versions + README | yes |
| goproxy | a file inside the artifact | yes | versions only | yes |
| pypi | the metadata document, else the artifact | yes | versions + README | no |
| conda | a file inside the artifact | yes | versions only | no |
| composer | a file inside the artifact | yes | versions only | yes |
| vscode-marketplace | a URL in the metadata, read separately | yes | versions + README | yes |
| maven | the POM carries `<description>`, which is a sentence rather than a document; putting one where a reader expects the other makes every package look thinly documented | — | versions only | no |
| terraform | a file inside the artifact | yes | versions only | no |
| rubygems | a file inside the artifact | yes | versions only | yes |
| nuget | a file inside the artifact | yes | versions only | yes |
| deb | path-addressed: there is no package identity to hang a README on | — | neither | no |
| rpm | path-addressed: there is no package identity to hang a README on | — | neither | no |
| pacman | path-addressed: there is no package identity to hang a README on | — | neither | no |
| jetbrains | path-addressed: there is no package identity to hang a README on | — | neither | no |
| jetbrains-marketplace | the metadata document, already fetched | yes | versions + README | yes |
| generic | path-addressed: there is no package identity to hang a README on | — | neither | no |
| nodedist | a Node release is a set of tarballs and a checksum file; the dist tree carries no prose | — | versions only | no |
| sdkman | SDKMAN describes a distribution, not a package: no document in the protocol carries prose about a candidate | — | versions only | no |
<!-- END readme-coverage -->

Configured per registry with
[`[registries.readme]`](/guide/admin-config#readme-capture) and
[`[registries.upstream_detail]`](/guide/admin-config#the-console-s-discovery-read).
Both are **on by default**, and the second one makes an outbound request the
first time somebody opens a package page for something this instance holds
nothing of — see [what leaves this
instance](/operations/egress#the-console-s-discovery-read).

Two settings shape what else the page can do: `remote_images = "proxy"` renders
a README's images through this server rather than charting them, and
`console_fetch` (on by default) is the *Fetch this version* button. Prose search
across stored READMEs is instance-wide and off by default —
[`[search] readmes`](/guide/admin-config#search-readmes).

## See also

- [User Guide](/use/) — task-oriented walkthroughs for the most common registries.
- [Administration → Configuration](/guide/admin-config#configuration) — how to declare registries in `config.toml`.
- [Caching](/guide/caching) · [Access Control](/guide/access-control) · [Roadmap](/guide/roadmap#new-registry-types).
