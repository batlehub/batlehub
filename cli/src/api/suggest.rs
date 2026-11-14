//! Derive the set of BatleHub registries a project needs from what it actually
//! downloads.
//!
//! Two inputs, in decreasing order of precision:
//!
//! 1. **`mise.lock`** — the best source there is: it records the *exact* URL of
//!    every tool the project installs, per platform. Every URL maps either onto
//!    a typed registry (a `github.com` release asset → `type = "github"`) or,
//!    when the host speaks no package protocol at all, onto a `generic` mirror
//!    of that host.
//! 2. **`mise.toml`** and the usual project manifests (`Cargo.toml`,
//!    `package.json`, …) — no URLs, so the mapping is by backend prefix / tool
//!    name and is necessarily best-effort.
//!
//! The output is a set of `[[registries]]` blocks plus the client-side
//! environment variables that point each toolchain at them. Suggestions are
//! deduplicated by registry name, so one `github` entry covers every aqua/ubi
//! tool in the lock.
//!
//! **On `path_allow` precision:** for hosts with a curated preset the allowlist
//! is a version-agnostic glob (`v*/**` for nodejs.org), so it keeps working
//! across version bumps. For unrecognised hosts the allowlist is the set
//! of exact paths observed in the lock — narrow and provably sufficient for the
//! pinned versions, but it needs widening (or a re-run of this command) when
//! those versions change. The emitted TOML says so inline.

use std::collections::BTreeMap;
use std::path::Path;

use batlehub_core::entities::RegistryKind;
use batlehub_core::services::mise_wire;

use crate::api::client_setup::NODEDIST_VARS;
use crate::api::setup::scan_project_types;

/// One registry BatleHub should be configured with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestedRegistry {
    /// Proposed `name` — also the segment in the proxy URL.
    pub name: String,
    /// Proposed `type`.
    pub registry_type: String,
    /// Proposed `upstreams`. Empty means "the type's built-in default is right".
    pub upstreams: Vec<String>,
    /// Proposed `path_allow` (path-addressed types only).
    pub path_allow: Vec<String>,
    /// Human-readable reasons this was suggested, e.g. `"mise.lock: node"`.
    pub sources: Vec<String>,
    /// Client-side environment variables, with `{proxy}` standing in for the
    /// registry's proxy base URL.
    pub client_env: Vec<(String, String)>,
    /// Extra caveat printed alongside the block.
    pub note: Option<String>,
    /// `[registries.cache] warm_packages` entries, `name@version`, from the
    /// files that pin a toolchain version (`.nvmrc`, `.sdkmanrc`).
    pub warm_packages: Vec<String>,
    /// `revalidate` globs: the preset's mutable documents (RFC 0048 §15 Q12).
    pub revalidate: Vec<String>,
    /// A per-registry `max_artifact_size_bytes`, for a preset whose artifacts
    /// outgrow the instance limit (RFC 0048 §15 Q11).
    pub max_artifact_size_bytes: Option<u64>,
}

impl SuggestedRegistry {
    /// The proxy base URL clients should point at, e.g.
    /// `https://hub.example.com/proxy/node-dist/generic`.
    ///
    /// The path-addressed types carry the type as a URL segment
    /// (`…/proxy/{name}/generic/{path}`), and so do the two toolchain kinds
    /// of RFC 0010 (`…/proxy/{name}/nodedist/index.tab`,
    /// `…/proxy/{name}/sdkman/candidates/all`). The other typed adapters
    /// route straight off the registry name — GitHub is
    /// `…/proxy/{name}/{owner}/{repo}/…`, with no `/github/` in the path.
    pub fn proxy_url(&self, server_url: &str) -> String {
        format!(
            "{}{}",
            server_url.trim_end_matches('/'),
            proxy_base_path(&self.name, &self.registry_type)
        )
    }

    /// `client_env` with `{proxy}` resolved against `server_url`.
    pub fn resolved_env(&self, server_url: &str) -> Vec<(String, String)> {
        let proxy = self.proxy_url(server_url);
        self.client_env
            .iter()
            .map(|(k, v)| (k.clone(), v.replace("{proxy}", &proxy)))
            .collect()
    }
}

// ── Host → registry mapping ───────────────────────────────────────────────────

/// A curated `generic` mirror for a host we know the layout of.
struct GenericPreset {
    name: &'static str,
    upstream: &'static str,
    path_allow: &'static [&'static str],
    revalidate: &'static [&'static str],
    max_artifact_size_bytes: Option<u64>,
}

/// Hosts that speak a package protocol BatleHub has a typed adapter for. The
/// suggested registry uses that adapter's default upstream, so no `upstreams`
/// entry is emitted.
const TYPED_HOSTS: &[(&str, &str, &str)] = &[
    // (host, registry type, suggested name)
    ("api.github.com", "github", "github"),
    // mise's versions host (RFC 0043): every registry tool is listed through it.
    ("mise-versions.jdx.dev", "mise-versions", "mise-versions"),
    ("github.com", "github", "github"),
    ("codeload.github.com", "github", "github"),
    ("raw.githubusercontent.com", "github", "github"),
    // The public Forgejo. A self-hosted one is not recognisable from a URL, so
    // its registry is written by hand — the rules below are codeberg's.
    ("codeberg.org", "forgejo", "forgejo"),
    ("objects.githubusercontent.com", "github", "github"),
    ("registry.npmjs.org", "npm", "npm"),
    ("crates.io", "cargo", "cargo"),
    ("static.crates.io", "cargo", "cargo"),
    ("index.crates.io", "cargo", "cargo"),
    ("pypi.org", "pypi", "pypi"),
    ("files.pythonhosted.org", "pypi", "pypi"),
    ("proxy.golang.org", "goproxy", "go"),
    // The Go *toolchain* (RFC 0048 phase 5); Go *modules* are `goproxy` above.
    // Keyed by path too: the rest of the host is Android's, which `godist`
    // cannot serve (RFC 0047 §6.1).
    ("dl.google.com/go/", "godist", "godist"),
    // The two toolchains with a typed kind of their own (RFC 0048 §6.2): a
    // block on a Node release or a Rust toolchain holds only through these.
    ("nodejs.org/dist/", "nodedist", "node"),
    ("static.rust-lang.org", "rustup", "rust"),
    ("rubygems.org", "rubygems", "rubygems"),
    ("repo1.maven.org", "maven", "maven"),
    ("registry.terraform.io", "terraform", "terraform"),
    ("api.nuget.org", "nuget", "nuget"),
    ("conda.anaconda.org", "conda", "conda"),
    ("repo.packagist.org", "composer", "composer"),
    ("packagist.org", "composer", "composer"),
    ("open-vsx.org", "openvsx", "openvsx"),
    (
        "marketplace.visualstudio.com",
        "vscode-marketplace",
        "vscode",
    ),
    ("download.jetbrains.com", "jetbrains", "jetbrains"),
    ("plugins.jetbrains.com", "jetbrains-marketplace", "jbm"),
];

/// The [`TYPED_HOSTS`] row for a URL's host and path. A key with a `/` is a
/// host plus a path prefix, and matches only under that prefix.
fn typed_host(
    host: &str,
    path: &str,
) -> Option<&'static (&'static str, &'static str, &'static str)> {
    TYPED_HOSTS
        .iter()
        .find(|(key, _, _)| match key.split_once('/') {
            Some((h, prefix)) => h == host && path.starts_with(prefix),
            None => *key == host,
        })
}

/// Hosts with no package protocol at all, for which we know a sensible upstream
/// root, allowlist and client mirror variable. Keyed by host and, where a host
/// carries more than one tree, the URL path (RFC 0047 §6.1: `dl.google.com`).
fn generic_preset(host: &str, path: &str) -> Option<GenericPreset> {
    // Every `path_allow` starts with a literal product segment, so a preset
    // never fronts the whole host (RFC 0047 §7.1 T2). `revalidate` lists the
    // documents a client re-reads for news; everything else is immutable.
    // Globs are matched with `*` inside one segment (`path_proxy.rs`).
    let preset = match host {
        // aqua `http` (boundary, consul, nomad, packer, terraform,
        // terraform-ls, vault, waypoint) and mise `http:` (nomad-pack,
        // sentinel, tfc-agent), whose version list is `{product}/index.json`.
        "releases.hashicorp.com" => GenericPreset {
            name: "hashicorp-releases",
            upstream: "https://releases.hashicorp.com",
            path_allow: &[
                "boundary/**",
                "consul/**",
                "nomad/**",
                "nomad-pack/**",
                "packer/**",
                "sentinel/**",
                "terraform/**",
                "terraform-ls/**",
                "tfc-agent/**",
                "vault/**",
                "waypoint/**",
            ],
            revalidate: &[
                "nomad-pack/index.json",
                "sentinel/index.json",
                "tfc-agent/index.json",
            ],
            max_artifact_size_bytes: None,
        },
        // The release dates of the three `http:` tools (`version_date_url`).
        // The generic route drops the query, so mise's `?limit=20&after=`
        // paging reads the first page (10 releases) and stops.
        "api.releases.hashicorp.com" => GenericPreset {
            name: "hashicorp-api",
            upstream: "https://api.releases.hashicorp.com/v1/releases",
            path_allow: &["nomad-pack", "sentinel", "tfc-agent"],
            revalidate: &["nomad-pack", "sentinel", "tfc-agent"],
            max_artifact_size_bytes: None,
        },
        // aqua kubectl and kubectl-convert: `{v}/bin/{os}/{arch}/kubectl*`,
        // with `.sha256`, `.sig` and `.cert` beside it.
        "dl.k8s.io" => GenericPreset {
            name: "k8s-dl",
            upstream: "https://dl.k8s.io",
            path_allow: &["v1.*/bin/*/*/kubectl*", "release/stable.txt"],
            revalidate: &["release/stable.txt"],
            max_artifact_size_bytes: None,
        },
        // aqua maven, spark, tomcat; vfox ant, groovy.
        "archive.apache.org" => GenericPreset {
            name: "apache-archive",
            upstream: "https://archive.apache.org/dist",
            path_allow: &["ant/**", "groovy/**", "maven/**", "spark/**", "tomcat/**"],
            revalidate: &[],
            max_artifact_size_bytes: None,
        },
        // mise `http:` oc and openshift-install, and their `sha256sum.txt`.
        "mirror.openshift.com" => GenericPreset {
            name: "openshift-mirror",
            upstream: "https://mirror.openshift.com/pub/openshift-v4/clients",
            path_allow: &[
                "ocp/*/openshift-client-*",
                "ocp/*/openshift-install-*",
                "ocp/*/sha256sum.txt",
            ],
            revalidate: &[],
            max_artifact_size_bytes: None,
        },
        // mise `http:` julia: `{os}/{arch}/{minor}/julia-{v}-…` and the
        // `versions.json` it lists and checksums from.
        "julialang-s3.julialang.org" => GenericPreset {
            name: "julia-dist",
            upstream: "https://julialang-s3.julialang.org/bin",
            path_allow: &["versions.json", "linux/**", "mac/**", "winnt/**"],
            revalidate: &["versions.json"],
            max_artifact_size_bytes: None,
        },
        // aqua docker-cli: `{os}/static/stable/{arch}/docker-{v}.{tgz,zip}`.
        "download.docker.com" => GenericPreset {
            name: "docker-dl",
            upstream: "https://download.docker.com",
            path_allow: &[
                "linux/static/stable/**",
                "mac/static/stable/**",
                "win/static/stable/**",
            ],
            revalidate: &[],
            max_artifact_size_bytes: None,
        },
        // vfox android-sdk (command-line tools from `repository2-3.xml`),
        // vfox aapt2 (Google's Maven) and mise `http:` android-cli. `/go/` on
        // the same host is the typed `godist` (`TYPED_HOSTS`).
        "dl.google.com" if path.starts_with("android/") => GenericPreset {
            name: "android-dl",
            upstream: "https://dl.google.com/android",
            path_allow: &[
                "cli/**",
                "maven2/com/android/tools/build/aapt2/**",
                "maven2/com/android/tools/build/group-index.xml",
                "repository/commandlinetools-*",
                "repository/repository2-3.xml",
            ],
            revalidate: &[
                "cli/latest/debian/dists/stable/Release",
                "cli/latest/debian/dists/stable/main/binary-amd64/Packages",
                "maven2/com/android/tools/build/group-index.xml",
                "repository/repository2-3.xml",
            ],
            max_artifact_size_bytes: None,
        },
        "get.helm.sh" => GenericPreset {
            name: "helm-bin",
            upstream: "https://get.helm.sh",
            path_allow: &["helm-v*"],
            revalidate: &[],
            max_artifact_size_bytes: None,
        },
        "binaries.sonarsource.com" => GenericPreset {
            name: "sonar-binaries",
            upstream: "https://binaries.sonarsource.com",
            path_allow: &["Distribution/**"],
            revalidate: &[],
            max_artifact_size_bytes: None,
        },
        // mise `core:deno`: `release/v{v}/deno-{triple}.zip` and its `.sha256sum`.
        "dl.deno.land" => GenericPreset {
            name: "deno-dl",
            upstream: "https://dl.deno.land",
            path_allow: &["release/**"],
            revalidate: &[],
            max_artifact_size_bytes: None,
        },
        // mise `core:zig`: `download/{v}/…` releases, `builds/…` nightlies,
        // each with a `.minisig`; mise reads the mirror list even with the
        // mirrors off, so it is served too.
        "ziglang.org" => GenericPreset {
            name: "zig-dl",
            upstream: "https://ziglang.org",
            path_allow: &["builds/**", "download/**"],
            revalidate: &["download/community-mirrors.txt", "download/index.json"],
            max_artifact_size_bytes: None,
        },
        // mise `core:elixir` and Linux `core:erlang`, each checked against its
        // directory's `builds.txt`.
        "builds.hex.pm" => GenericPreset {
            name: "hex-builds",
            upstream: "https://builds.hex.pm/builds",
            path_allow: &["elixir/**", "otp/**"],
            revalidate: &["elixir/builds.txt", "otp/*/*/builds.txt"],
            max_artifact_size_bytes: None,
        },
        // mise `core:swift`: `swift-{v}-release/{platform}/…` plus `.sig`. The
        // directory is per patch release, so the glob, not a list, holds it.
        "download.swift.org" => GenericPreset {
            name: "swift-dl",
            upstream: "https://download.swift.org",
            path_allow: &["swift-*-release/**"],
            revalidate: &[],
            // Measured 2026-11-14: 1 124 819 616 B (6.4.2 Linux), 1 591 283 484 B
            // (macOS pkg), against the 500 MiB instance default. Windows'
            // 2.1 GB installer stays refused.
            max_artifact_size_bytes: Some(1_700_000_000),
        },
        // The release index mise reads to pick a Linux build (`swift.rs:543`):
        // one document, so the upstream is its directory, not the website.
        "www.swift.org" => GenericPreset {
            name: "swift-api",
            upstream: "https://www.swift.org/api/v1/install",
            path_allow: &["releases.json"],
            revalidate: &["releases.json"],
            max_artifact_size_bytes: None,
        },
        // PyPy, a `core:python` flavour (`python.rs:38`): one flat directory
        // of `pypy{series}-v{v}-{platform}` archives beside `versions.json`.
        "downloads.python.org" => GenericPreset {
            name: "pypy-dl",
            upstream: "https://downloads.python.org/pypy",
            path_allow: &["pypy2.7-v*", "pypy3.*-v*", "versions.json"],
            revalidate: &["versions.json"],
            max_artifact_size_bytes: None,
        },
        _ => return None,
    };
    Some(preset)
}

/// Hosts whose preset the CLI no longer writes because a typed kind serves
/// them (RFC 0048 §6.2), with the upstream that preset had. An estate that
/// already declared such a `generic` mirror still plans through it.
const RETIRED_GENERIC: &[(&str, &str)] = &[
    ("nodejs.org", "https://nodejs.org/dist"),
    ("static.rust-lang.org", "https://static.rust-lang.org"),
];

/// The upstream a `generic` mirror of `host` (under `path`) is assumed to have.
fn generic_upstream(host: &str, path: &str) -> Option<&'static str> {
    generic_preset(host, path).map(|p| p.upstream).or_else(|| {
        RETIRED_GENERIC
            .iter()
            .find(|(h, _)| *h == host)
            .map(|(_, u)| *u)
    })
}

/// The client variables a typed toolchain kind is pointed at its registry
/// with, `{proxy}` standing for the registry's proxy base.
fn typed_client_env(registry_type: &str) -> Vec<(String, String)> {
    match registry_type {
        "nodedist" => NODEDIST_VARS
            .iter()
            .map(|k| ((*k).to_owned(), "{proxy}".to_owned()))
            .collect(),
        "rustup" => vec![
            ("RUSTUP_DIST_SERVER".to_owned(), "{proxy}/rustup".to_owned()),
            (
                "RUSTUP_UPDATE_ROOT".to_owned(),
                "{proxy}/rustup/rustup".to_owned(),
            ),
        ],
        _ => Vec::new(),
    }
}

/// Object-storage hosts where the first path segment is a tenant bucket. The
/// bucket must be part of the upstream root, otherwise the mirror would relay
/// every other public bucket on the same host.
const BUCKET_HOSTS: &[&str] = &[
    "storage.googleapis.com",
    "s3.amazonaws.com",
    "objects.githubusercontent.com",
];

/// The version lists and release manifests mise 2026.11.3's registry names on
/// a bucket host (`registry/*.toml`). A bucket mirror admits, and revalidates,
/// the ones under its own bucket. dart's (the GCS JSON API, another path) and
/// grok's (`x.ai`) are not under theirs, so they are not here.
const BUCKET_LIST_DOCUMENTS: &[&str] = &[
    "https://storage.googleapis.com/claude-code-dist-86c565f3-f756-42ad-8dfa-d59b1c096819/claude-code-releases/latest",
    "https://storage.googleapis.com/flutter_infra_release/releases/releases_linux.json",
    "https://storage.googleapis.com/flutter_infra_release/releases/releases_macos.json",
    "https://storage.googleapis.com/flutter_infra_release/releases/releases_windows.json",
];

/// mise backend prefixes that imply a registry even when the lock records no
/// URL (`cargo:`, `pipx:` and friends install from the ecosystem registry).
/// The path this download takes through BatleHub, or `None` when nothing
/// mirrors its host (RFC 0008 §4.2).
///
/// A **path**, not a full URL: a plan built against one server is imported on
/// another, and a host baked into every entry would make the plan a statement
/// about the machine that produced it rather than about the estate.
///
/// The rewrite is the same table `--mise` emits — one source, so what the
/// seed fetches is exactly what mise would fetch. That is the property that
/// makes a successful seed evidence the rewrite table is right, which is what
/// `unmirrored_hosts` cannot prove on its own.
pub(crate) fn proxy_path_for(
    url: &str,
    registry_name: &str,
    registry_type: &str,
) -> Option<String> {
    // The same base the rewrite rules use, type segment and all. Without it
    // a `generic` mirror plans `/proxy/{name}/v24/node.tar.gz` while the
    // route is `/proxy/{name}/generic/v24/node.tar.gz`, and every seeded
    // entry 404s — the path-addressed kinds are exactly the ones RFC 0008's
    // toolchain estate leans on.
    let proxy = proxy_base_path(registry_name, registry_type);
    if registry_type == "generic" {
        let (host, path) = split_url(url)?;
        let upstream = generic_upstream(host, path)?.trim_end_matches('/');
        let rest = url.strip_prefix(upstream)?.trim_start_matches('/');
        return Some(format!("{proxy}/{rest}"));
    }
    for (pattern, replacement) in typed_url_replacements(registry_type) {
        let Ok(re) = regex::Regex::new(pattern) else {
            continue;
        };
        if let Some(caps) = re.captures(url) {
            let mut out = replacement.replace("{proxy}", &proxy);
            // `$1`..`$9`, highest first so `$10` is never eaten by `$1`.
            for i in (1..=9).rev() {
                let group = caps.get(i).map(|m| m.as_str()).unwrap_or("");
                out = out.replace(&format!("${i}"), group);
            }
            return Some(out);
        }
    }
    None
}

/// `/proxy/{name}` plus the type segment the path-addressed kinds carry.
///
/// One rule, shared by the URL rewrite rules ([`SuggestedRegistry::proxy_url`])
/// and by the plan's own paths, because a plan whose paths disagree with the
/// rules it emits is a bundle seeded from 404s.
pub(crate) fn proxy_base_path(registry_name: &str, registry_type: &str) -> String {
    match registry_type.parse::<RegistryKind>() {
        Ok(kind) => mise_wire::proxy_base_path(registry_name, kind),
        Err(_) => format!("/proxy/{registry_name}"),
    }
}

/// The registry *kinds* a host's downloads may go through, preferred first,
/// for RFC 0008's plan.
///
/// Same table the suggester reads, asked a narrower question: planning needs
/// to know whether *some* configured registry of such a kind mirrors the
/// host, not what a new registry should be called.
pub(crate) fn registry_kinds_for_host(host: &str, path: &str) -> Vec<&'static str> {
    let mut kinds: Vec<&'static str> = typed_host(host, path)
        .map(|(_, k, _)| *k)
        .into_iter()
        .collect();
    // A generic mirror can front any host, so a host with a preset is
    // mirrorable as `generic` — and one without is not mirrorable at all
    // until an operator writes a registry for it. Node and Rust keep the
    // `generic` answer behind their typed kind, for the estates that
    // declared the retired presets.
    if generic_upstream(host, path).is_some() {
        kinds.push("generic");
    }
    kinds
}

/// Why a backend cannot come through BatleHub, or `None` when it can.
///
/// A backend that downloads over HTTP is covered by the URL rules; one that
/// shells out to the ecosystem's own tool is covered by the client-env
/// block. What is left is the backends that fetch over git, and the estate
/// learns that here — at planning time, naming the backend — rather than at
/// install time on a disconnected workstation, naming a connect timeout.
pub(crate) fn unsupported_backend_reason(backend: &str) -> Option<String> {
    if BACKEND_REGISTRIES
        .iter()
        .any(|(prefix, _, _)| backend.starts_with(prefix))
        || predicate_registry(backend).is_some()
    {
        return None;
    }
    if let Some(host) = packslip_host(backend) {
        return Some(format!(
            "{backend}: reads https://{host}/.well-known/packslip.json; no registry mirrors {host}"
        ));
    }
    // vfox plugins built into mise download over HTTP, so the URL rules
    // cover them; the plan carries a note instead (RFC 0047 §4.2).
    if backend.starts_with("vfox:") {
        return None;
    }
    if backend.starts_with("asdf:") {
        return Some(format!(
            "{backend}: a git-fetched plugin backend; mise clones it, and there is no HTTP path \
             through BatleHub"
        ));
    }
    if let Some(reason) = core_refusal(backend) {
        return Some(format!("{backend}: {reason}"));
    }
    // Called for a lock entry with no URL: these four record one whenever
    // they install a precompiled build, so none means mise compiles.
    if let Some(tool) = ["core:node", "core:python", "core:ruby", "core:erlang"]
        .into_iter()
        .find(|b| *b == backend)
        .and_then(|b| b.strip_prefix("core:"))
    {
        return Some(format!(
            "{backend}: the lock records no download URL, so mise compiles it \
             ({tool}.compile) with tools it clones and runs as subprocesses, which no rewrite \
             rule reaches; run `mise lock` if this install is precompiled"
        ));
    }
    if backend.starts_with("core:") {
        // `core:*` downloads from a fixed host, which the URL rules cover
        // when a registry fronts it — and `unmirrored_hosts` says so when
        // none does.
        return None;
    }
    Some(format!(
        "{backend}: no registry kind fronts this backend, and it downloads nothing over HTTP that \
         a rewrite rule could catch"
    ))
}

const BACKEND_REGISTRIES: &[(&str, &str, &str)] = &[
    // (backend prefix, registry type, suggested name)
    ("cargo:", "cargo", "cargo"),
    ("pipx:", "pypi", "pypi"),
    ("pypi:", "pypi", "pypi"),
    ("npm:", "npm", "npm"),
    ("go:", "goproxy", "go"),
    ("gem:", "rubygems", "rubygems"),
    // mise drives rattler in-process, so this one needs the URL rule below too.
    ("conda:", "conda", "conda"),
    // aqua and ubi both resolve to GitHub release assets.
    ("aqua:", "github", "github"),
    ("ubi:", "github", "github"),
    ("github:", "github", "github"),
    // mise reads the version list itself and installs with `dotnet tool
    // install` (RFC 0047 §4.2), both against a NuGet feed.
    ("dotnet:", "nuget", "nuget"),
    ("packslip:github.com/", "github", "github"),
];

/// Where each `core:` toolchain downloads from, for a lock entry that records
/// no URL (`core:rust` and `core:swift` never do) or a `mise.toml` with no
/// lock: resolved through [`registry_for_url`] like a locked URL (RFC 0048
/// §4.2). Absent: `core:python`, `core:bun` and `core:ruby` resolve through
/// their locked GitHub URLs, `core:java` waits for the `mise-java` kind
/// (phase 2), `core:dotnet` is refused ([`core_refusal`]).
const CORE_ROUTES: &[(&str, &[&str])] = &[
    ("core:node", &["https://nodejs.org/dist/"]),
    ("core:rust", &["https://static.rust-lang.org/"]),
    ("core:go", &["https://dl.google.com/go/"]),
    ("core:deno", &["https://dl.deno.land/release/"]),
    ("core:zig", &["https://ziglang.org/download/"]),
    ("core:elixir", &["https://builds.hex.pm/builds/elixir/"]),
    // Linux; a macOS lock names `erlef/otp_builds` on GitHub by URL.
    ("core:erlang", &["https://builds.hex.pm/builds/otp/"]),
    (
        "core:swift",
        &[
            "https://download.swift.org/",
            "https://www.swift.org/api/v1/install/releases.json",
        ],
    ),
];

/// Why a `core:` toolchain cannot come through BatleHub at all.
fn core_refusal(backend: &str) -> Option<&'static str> {
    // `dotnet-install.sh` hard-codes its feeds and takes them only from
    // `--azure-feed`, which mise does not pass (`dotnet.rs:389-405`).
    (backend == "core:dotnet").then_some(
        "mise runs dotnet-install.sh, whose feed mise does not let you set; no rewrite rule \
         reaches it",
    )
}

/// Backends whose registry is not a prefix of the name: `packslip:{owner}/{repo}`
/// is GitHub (no dot in the first segment, so not a host), and `spm:` is
/// GitLab when the name says so (mise tests `contains("gitlab.com")`,
/// `src/backend/spm.rs:812`), GitHub otherwise.
fn predicate_registry(backend: &str) -> Option<&'static str> {
    if let Some(rest) = backend.strip_prefix("packslip:") {
        let first = rest.split('/').next().unwrap_or_default();
        return (rest.contains('/') && !first.contains('.')).then_some("github");
    }
    if let Some(rest) = backend.strip_prefix("spm:") {
        return Some(if rest.contains("gitlab.com") {
            "gitlab"
        } else {
            "github"
        });
    }
    None
}

/// The host of a `packslip:{host}[/…]` backend, which mirrors nothing here.
fn packslip_host(backend: &str) -> Option<&str> {
    let rest = backend.strip_prefix("packslip:")?;
    if rest.starts_with("github.com/") || predicate_registry(backend).is_some() {
        return None;
    }
    let host = rest.split('/').next().unwrap_or_default();
    (!host.is_empty()).then_some(host)
}

/// vfox plugins built into mise that shell out, and what the subprocess
/// fetches past `url_replacements` (RFC 0047 §4.2, §15 Q4). Keyed by the
/// plugin's short name. shortcut: a list that drifts; re-measure it on every
/// mise pin bump by grepping the embedded plugins' Lua for `os.execute` and
/// `cmd.exec` — a missing entry loses the warning, never a route.
const VFOX_SUBPROCESS_PLUGINS: &[(&str, &str)] = &[
    ("php", "configure, make"),
    ("poetry", "the installer script, run by python"),
    ("pipenv", "pip"),
];

/// The note for a `vfox:` plugin known to shell out, locked or not.
pub(crate) fn vfox_subprocess_note(backend: &str) -> Option<String> {
    let plugin = backend.strip_prefix("vfox:")?;
    let short = plugin.rsplit('/').next().unwrap_or(plugin);
    let short = short.strip_prefix("vfox-").unwrap_or(short);
    let (_, what) = VFOX_SUBPROCESS_PLUGINS.iter().find(|(p, _)| *p == short)?;
    Some(format!(
        "{backend}: runs a subprocess ({what}) whose own downloads bypass url_replacements; \
         connected they go direct, air-gapped they fail"
    ))
}

/// The note for a backend the plan resolved without a URL: a `vfox:` plugin
/// downloads over HTTP once the lock names the URL.
pub(crate) fn lockless_backend_note(backend: &str) -> Option<String> {
    backend.starts_with("vfox:").then(|| {
        format!(
            "{backend}: built into mise, downloads over HTTP; run `mise lock` so the plan knows the URL"
        )
    })
}

/// The caveat a lock-less suggestion carries above its registry.
fn backend_suggestion_note(backend: &str) -> Option<String> {
    if backend.starts_with("aqua:") {
        return Some(
            "aqua packages of type `http` download from their vendor's host, not GitHub; \
             run `mise lock` to see which."
                .to_owned(),
        );
    }
    if backend.starts_with("spm:") {
        return Some(
            "spm builds from source and clones over git, which does not route.".to_owned(),
        );
    }
    None
}

// ── Accumulation ──────────────────────────────────────────────────────────────

/// Append the items of `incoming` that `into` does not already hold.
///
/// Order-preserving and quadratic, which is what the three merged lists want:
/// they are short, and the order they were discovered in is the order they read
/// best in.
fn merge_new<T: PartialEq>(into: &mut Vec<T>, incoming: impl Iterator<Item = T>) {
    for item in incoming {
        if !into.contains(&item) {
            into.push(item);
        }
    }
}

/// Collects suggestions keyed by registry name, merging sources and allowlists
/// so that (say) ten aqua tools produce one `github` entry.
#[derive(Default)]
struct Accumulator {
    by_name: BTreeMap<String, SuggestedRegistry>,
    /// `tool (backend) — reason` for each tool no registry can route.
    unroutable: Vec<String>,
}

impl Accumulator {
    fn add(&mut self, mut reg: SuggestedRegistry) {
        let Some(existing) = self.by_name.get_mut(&reg.name) else {
            self.by_name.insert(reg.name.clone(), reg);
            return;
        };
        merge_new(&mut existing.sources, reg.sources.drain(..));
        merge_new(&mut existing.path_allow, reg.path_allow.drain(..));
        merge_new(&mut existing.upstreams, reg.upstreams.drain(..));
        merge_new(&mut existing.client_env, reg.client_env.drain(..));
        merge_new(&mut existing.warm_packages, reg.warm_packages.drain(..));
        merge_new(&mut existing.revalidate, reg.revalidate.drain(..));
        existing.max_artifact_size_bytes = existing
            .max_artifact_size_bytes
            .or(reg.max_artifact_size_bytes);
        if existing.note.is_none() {
            existing.note = reg.note.take();
        }
    }

    fn refuse(&mut self, tool: &str, backend: &str) -> bool {
        let Some(reason) = core_refusal(backend) else {
            return false;
        };
        let line = format!("{tool} ({backend}) — {reason}");
        if !self.unroutable.contains(&line) {
            self.unroutable.push(line);
        }
        true
    }

    fn finish(self) -> Vec<SuggestedRegistry> {
        let mut out: Vec<SuggestedRegistry> = self.by_name.into_values().collect();
        // Typed registries first (they need no allowlist tuning), then generic
        // mirrors; alphabetical within each group.
        out.sort_by(|a, b| {
            let key = |r: &SuggestedRegistry| (r.registry_type == "generic", r.name.clone());
            key(a).cmp(&key(b))
        });
        for reg in &mut out {
            reg.path_allow.sort();
            reg.sources.sort();
            reg.warm_packages.sort();
        }
        out
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Scan `root` and return the registries the project's downloads imply.
#[cfg(test)]
pub fn suggest_registries(root: &Path, depth: usize) -> Vec<SuggestedRegistry> {
    suggest(root, depth).0
}

/// The registries `root`'s downloads imply, and the tools no registry can
/// route, each as `tool (backend) — reason`.
pub fn suggest(root: &Path, depth: usize) -> (Vec<SuggestedRegistry>, Vec<String>) {
    let mut acc = Accumulator::default();

    let lock = root.join("mise.lock");
    let had_lock = lock.exists();
    if had_lock {
        if let Ok(content) = std::fs::read_to_string(&lock) {
            collect_from_mise_lock(&content, &mut acc);
        }
    }
    // The lock supersedes mise.toml: it names the same tools with exact URLs, so
    // re-deriving them from tool names would only add fuzzier duplicates.
    if !had_lock {
        for name in ["mise.toml", ".mise.toml"] {
            let path = root.join(name);
            if let Ok(content) = std::fs::read_to_string(&path) {
                collect_from_mise_toml(&content, name, &mut acc);
                break;
            }
        }
    }

    collect_from_manifests(root, depth, &mut acc);
    collect_from_toolchain_files(root, &mut acc);
    collect_from_ansible_files(root, &mut acc);
    collect_from_helm_chart(root, &mut acc);
    collect_from_flake(root, &mut acc);

    let unroutable = std::mem::take(&mut acc.unroutable);
    (acc.finish(), unroutable)
}

// ── .nvmrc and .sdkmanrc ──────────────────────────────────────────────────────
//
// RFC 0010 §6.9. Each pins the toolchain a project builds with, in a file the
// manager itself reads: `.nvmrc` is one line — a version or an alias — and
// `.sdkmanrc` is `candidate=version` pairs. Both map onto a typed registry
// and the client variable that points the manager at it, and a pinned
// version becomes a `warm_packages` entry so the proxy holds the bytes before
// the first `nvm install` / `sdk env install` asks for them. Neither file
// names a platform; warming reads `warm_platforms` for that, defaulting to the
// server's own.

/// `.nvmrc` → a `nodedist` registry; `.sdkmanrc` → an `sdkman` one.
fn collect_from_toolchain_files(root: &Path, acc: &mut Accumulator) {
    if let Ok(content) = std::fs::read_to_string(root.join(".nvmrc")) {
        acc.add(registry_for_nvmrc(&content));
    }
    if let Ok(content) = std::fs::read_to_string(root.join(".sdkmanrc")) {
        acc.add(registry_for_sdkmanrc(&content));
    }
}

/// The `nodedist` registry an `.nvmrc` implies. A concrete version is warmed
/// as the tree spells it (`v22.11.0`); an alias (`lts/*`, `node`, `lts/jod`)
/// names no release and is left to `nvm ls-remote`.
fn registry_for_nvmrc(content: &str) -> SuggestedRegistry {
    let pin = content
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'));
    let warm_packages = match pin {
        Some(v) if v.chars().next().is_some_and(|c| c.is_ascii_digit()) => {
            vec![format!("node@v{v}")]
        }
        Some(v)
            if v.starts_with('v') && v[1..].chars().next().is_some_and(|c| c.is_ascii_digit()) =>
        {
            vec![format!("node@{v}")]
        }
        _ => Vec::new(),
    };
    let note = match pin {
        Some(v) if warm_packages.is_empty() => Some(format!(
            ".nvmrc pins the alias '{v}', which names no release; nvm resolves it through \
             index.tab, so nothing is warmed ahead of time"
        )),
        _ => None,
    };
    SuggestedRegistry {
        name: "node".to_owned(),
        registry_type: "nodedist".to_owned(),
        upstreams: Vec::new(),
        path_allow: Vec::new(),
        sources: vec![".nvmrc".to_owned()],
        client_env: typed_client_env("nodedist"),
        warm_packages,
        revalidate: Vec::new(),
        max_artifact_size_bytes: None,
        note,
    }
}

/// The `sdkman` registry an `.sdkmanrc` implies, one `warm_packages` entry per
/// `candidate=version` line.
fn registry_for_sdkmanrc(content: &str) -> SuggestedRegistry {
    let warm_packages = content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(c, v)| format!("{}@{}", c.trim(), v.trim()))
        .filter(|entry| !entry.starts_with('@') && !entry.ends_with('@'))
        .collect();
    SuggestedRegistry {
        name: "sdkman".to_owned(),
        registry_type: "sdkman".to_owned(),
        upstreams: Vec::new(),
        path_allow: Vec::new(),
        sources: vec![".sdkmanrc".to_owned()],
        // Both variables are read by sdkman-init.sh only when empty, so they
        // have to be exported before it is sourced.
        client_env: vec![
            ("SDKMAN_CANDIDATES_API".to_owned(), "{proxy}".to_owned()),
            ("SDKMAN_BROKER_API".to_owned(), "{proxy}/broker".to_owned()),
        ],
        warm_packages,
        revalidate: Vec::new(),
        max_artifact_size_bytes: None,
        note: None,
    }
}

// ── requirements.yml and galaxy.yml ──────────────────────────────────────────
//
// RFC 0031 §6.9. Three files name an Ansible dependency: `requirements.yml` and
// `collections/requirements.yml` list what a play installs, and `galaxy.yml`
// describes a collection being developed here and what it depends on. All three
// map onto one `galaxy` registry, and a pinned collection becomes a
// `warm_packages` entry so the proxy holds the tarball before the first
// `ansible-galaxy collection install` asks for it.
//
// **The reader is deliberately narrow.** This workspace has no YAML parser and
// is not gaining one for three files, so what is read is the block form every
// tool that writes these files produces:
//
// ```yaml
// collections:
//   - name: community.general
//     version: ">=13.0.0"
//   - community.docker
// ```
//
// A flow-style document (`collections: [ ... ]`) is not read, and the
// suggestion says so rather than silently emitting a registry with nothing
// warmed.

/// `requirements.yml`, `collections/requirements.yml` and `galaxy.yml` → one
/// `galaxy` registry.
fn collect_from_ansible_files(root: &Path, acc: &mut Accumulator) {
    let candidates = [
        ("requirements.yml", root.join("requirements.yml")),
        (
            "collections/requirements.yml",
            root.join("collections").join("requirements.yml"),
        ),
        ("galaxy.yml", root.join("galaxy.yml")),
    ];
    let mut sources = Vec::new();
    let mut warm: Vec<String> = Vec::new();
    let mut unreadable = false;

    for (label, path) in candidates {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        sources.push(label.to_owned());
        let (pins, flow) = if label == "galaxy.yml" {
            (galaxy_yml_dependencies(&content), false)
        } else {
            requirements_collections(&content)
        };
        unreadable |= flow;
        for pin in pins {
            if !warm.contains(&pin) {
                warm.push(pin);
            }
        }
    }
    if sources.is_empty() {
        return;
    }

    let note = if unreadable {
        Some(
            "a collections list in flow style ('collections: [...]') was not read; add its \
             pinned versions to warm_packages by hand"
                .to_owned(),
        )
    } else if warm.is_empty() {
        Some(
            "no collection is pinned to an exact version, so nothing is warmed ahead of time; \
             ansible-galaxy resolves the range through the versions list"
                .to_owned(),
        )
    } else {
        None
    };

    acc.add(SuggestedRegistry {
        name: "galaxy".to_owned(),
        registry_type: "galaxy".to_owned(),
        upstreams: Vec::new(),
        path_allow: Vec::new(),
        sources,
        // `ansible.cfg` is the documented switch, and these are the environment
        // overrides for the same two settings — which is what a CI job sets.
        // The server name in the second variable is the one named in the first.
        client_env: vec![
            (
                "ANSIBLE_GALAXY_SERVER_LIST".to_owned(),
                "batlehub".to_owned(),
            ),
            (
                "ANSIBLE_GALAXY_SERVER_BATLEHUB_URL".to_owned(),
                "{proxy}/api/".to_owned(),
            ),
        ],
        warm_packages: warm,
        revalidate: Vec::new(),
        max_artifact_size_bytes: None,
        // `roles` is written out rather than left to the default, so the egress
        // to github.com it implies is a choice visible in the file the operator
        // commits (RFC 0031 decision 9).
        note,
    });
}

// ── Chart.yaml and Chart.lock ────────────────────────────────────────────────
//
// RFC 0029 §6.9. A chart's `dependencies[].repository` names the chart
// repositories `helm dependency update` reads. A registry serves exactly one
// repository, so each `https://` repository is its own `helm` registry with
// that repository as its one upstream. `Chart.lock` is read in preference: it
// names the same repositories with the versions actually resolved, which is
// what `warm_packages` wants.

/// `Chart.lock`, else `Chart.yaml` → one `helm` registry per chart repository.
fn collect_from_helm_chart(root: &Path, acc: &mut Accumulator) {
    let Some((source, content)) = ["Chart.lock", "Chart.yaml"]
        .into_iter()
        .find_map(|f| Some((f, std::fs::read_to_string(root.join(f)).ok()?)))
    else {
        return;
    };
    let Ok(doc) = batlehub_core::services::helm::parse_yaml(&content) else {
        return;
    };
    let deps = doc
        .get("dependencies")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    // repository → the exact pins under it, in the order they were read.
    let mut repos: Vec<(String, Vec<String>)> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for dep in deps {
        let text = |k: &str| match dep.get(k) {
            Some(serde_json::Value::String(s)) => Some(s.trim().to_owned()),
            Some(serde_json::Value::Number(n)) => Some(n.to_string()),
            _ => None,
        };
        let (Some(name), Some(repo)) = (text("name"), text("repository")) else {
            continue;
        };
        if !(repo.starts_with("https://") || repo.starts_with("http://")) {
            // `oci://` is a container registry, `@alias`/`alias:` names a repo
            // only the developer's own `helm repo add` knows, `file://` is local.
            skipped.push(format!("{name} ({repo})"));
            continue;
        }
        let repo = repo.trim_end_matches('/').to_owned();
        let pin = text("version")
            .filter(|v| is_exact_chart_version(v))
            .map(|v| format!("{name}@{v}"));
        match repos.iter_mut().find(|(r, _)| *r == repo) {
            Some((_, pins)) => merge_new(pins, pin.into_iter()),
            None => repos.push((repo, pin.into_iter().collect())),
        }
    }

    // ponytail: the note rides each emitted registry; a chart whose every
    // dependency is skipped gets no registry, so its note is not printed.
    let note = (!skipped.is_empty()).then(|| {
        format!(
            "{source}: not proxied — {}. A helm registry fronts a classic https:// chart \
             repository; oci:// is a container registry, and an alias or file:// names nothing \
             this instance can reach",
            skipped.join(", ")
        )
    });
    for (repo, pins) in repos {
        // `http://x` and `https://x`, or `…/a-b` and `…/a/b`, fold to one name;
        // a helm registry takes one upstream, so the second gets a suffix.
        let base = helm_registry_name(&repo);
        let name = (1..)
            .map(|n| {
                if n == 1 {
                    base.clone()
                } else {
                    format!("{base}-{n}")
                }
            })
            .find(|n| {
                acc.by_name
                    .get(n)
                    .is_none_or(|r| r.upstreams == [repo.clone()])
            })
            .expect("an unused name");
        acc.add(SuggestedRegistry {
            name,
            registry_type: "helm".to_owned(),
            upstreams: vec![repo],
            path_allow: Vec::new(),
            sources: vec![source.to_owned()],
            // `helm repo add <name> {proxy}` is the switch; helm reads no
            // environment variable for a repository URL.
            client_env: Vec::new(),
            warm_packages: pins,
            revalidate: Vec::new(),
            max_artifact_size_bytes: None,
            note: note.clone(),
        });
    }
}

/// `https://charts.jetstack.io` → `helm-charts-jetstack-io`: one registry per
/// repository, so the name has to tell two of them apart.
fn helm_registry_name(repo: &str) -> String {
    let tail = repo.split_once("://").map_or(repo, |(_, t)| t);
    let mut name = String::from("helm-");
    for c in tail.chars().map(|c| c.to_ascii_lowercase()) {
        if c.is_ascii_alphanumeric() {
            name.push(c);
        } else if !name.ends_with('-') {
            name.push('-');
        }
    }
    name.trim_end_matches('-').to_owned()
}

/// A version `helm` resolves to exactly one release: `1.2.3` or `v1.2.3`, with
/// an optional pre-release or build suffix. `1.2.x`, `^1.2`, `>=1 <2` are ranges.
///
/// The three-number test alone reads only up to the first `-`, so
/// `1.0.0-rc.1 || 2.0.0` passed as exact; helm's own grammar
/// (`canonical_version`, a full match) is what rejects the tail.
fn is_exact_chart_version(v: &str) -> bool {
    if batlehub_core::services::helm::canonical_version(v).is_none() {
        return false;
    }
    let core = v.strip_prefix('v').unwrap_or(v);
    let core = core.split(['-', '+']).next().unwrap_or("");
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

// ── flake.nix and flake.lock ─────────────────────────────────────────────────
//
// RFC 0028 §6.9. A flake is the sign that this project builds with Nix, and a
// machine that builds with Nix substitutes from a binary cache — which is the
// registry to suggest.
//
// **Nothing is warmed, and that is a property of the ecosystem rather than a
// gap here.** A flake names *inputs* (nixpkgs at a revision, other flakes); it
// names no store paths. What a build will substitute is the output of an
// evaluation that needs a Nix this CLI does not have and a nixpkgs checkout it
// does not want. So the note says which setting takes store paths — the
// registry's own `warm_paths` — rather than emitting an empty `warm_packages`
// that reads like a resolver failure.

/// `flake.nix` / `flake.lock` → one `nix` registry.
fn collect_from_flake(root: &Path, acc: &mut Accumulator) {
    let sources: Vec<String> = ["flake.nix", "flake.lock"]
        .into_iter()
        .filter(|f| root.join(f).exists())
        .map(str::to_owned)
        .collect();
    if sources.is_empty() {
        return;
    }

    acc.add(SuggestedRegistry {
        name: "nix".to_owned(),
        registry_type: "nix".to_owned(),
        upstreams: Vec::new(),
        path_allow: Vec::new(),
        sources,
        // Nix has no environment override for `substituters`: the setting is
        // read from `nix.conf`, from a flake's own `nixConfig`, or from
        // `--option` on the command line. `NIX_CONFIG` carries whole
        // configuration lines, which is the one variable a CI job can set, so
        // that is what is suggested.
        client_env: vec![("NIX_CONFIG".to_owned(), "substituters = {proxy}".to_owned())],
        warm_packages: Vec::new(),
        revalidate: Vec::new(),
        max_artifact_size_bytes: None,
        note: Some(
            "a flake names inputs, not store paths, so nothing can be warmed from it — the \
             paths a build substitutes are the output of an evaluation. Pre-fetch a closure \
             with `[registries.cache] warm_paths`, which takes /nix/store entries. Every \
             machine also needs cache.nixos.org's key in trusted-public-keys: this proxy \
             relays upstream signatures and signs nothing it proxied"
                .to_owned(),
        ),
    });
}

/// The exactly-pinned collections a `requirements.yml` names, and whether the
/// document used a shape this reader does not understand.
///
/// A range (`>=13.0.0`, `*`) is not warmed: it names no release, and
/// `ansible-galaxy` resolves it through the versions list.
fn requirements_collections(content: &str) -> (Vec<String>, bool) {
    let mut pins = Vec::new();
    let mut in_collections = false;
    let mut flow = false;
    // The `name:` of the list entry currently being read, waiting for a
    // `version:` on one of its continuation lines.
    let mut pending: Option<String> = None;

    for raw in content.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // A top-level key ends whatever block we were in.
        if !line.starts_with(' ') && !line.starts_with('-') {
            pending = None;
            in_collections = trimmed.starts_with("collections:");
            if in_collections && trimmed != "collections:" {
                // `collections: [ ... ]` — flow style, which this reader does
                // not understand and says so rather than reporting nothing.
                flow = true;
                in_collections = false;
            }
            continue;
        }
        if !in_collections {
            continue;
        }
        if let Some(entry) = trimmed.strip_prefix("- ") {
            // A new entry: whatever the previous one was, it named no version.
            pending = entry
                .trim()
                .strip_prefix("name:")
                .map(|name| unquote(name).to_owned());
            continue;
        }
        if let Some(version) = trimmed.strip_prefix("version:") {
            let version = unquote(version);
            if let Some(name) = pending.take() {
                if is_exact_version(version) {
                    pins.push(format!("{name}@{version}"));
                }
            }
        }
    }
    (pins, flow)
}

/// The exactly-pinned dependencies a `galaxy.yml` declares, under
/// `dependencies:` as `name: version` pairs.
fn galaxy_yml_dependencies(content: &str) -> Vec<String> {
    let mut pins = Vec::new();
    let mut in_deps = false;
    for raw in content.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('-') {
            in_deps = trimmed.starts_with("dependencies:") && trimmed == "dependencies:";
            continue;
        }
        if !in_deps {
            continue;
        }
        if let Some((name, version)) = trimmed.split_once(':') {
            let name = unquote(name);
            let version = unquote(version);
            if name.contains('.') && is_exact_version(version) {
                pins.push(format!("{name}@{version}"));
            }
        }
    }
    pins
}

/// A version string that names one release rather than a range.
///
/// `ansible-galaxy` accepts the full semver range grammar; anything carrying a
/// comparator, a wildcard or a comma is a range and warming cannot name the
/// release it will resolve to.
fn is_exact_version(v: &str) -> bool {
    !v.is_empty()
        && v != "*"
        && v.chars().next().is_some_and(|c| c.is_ascii_digit())
        && !v.contains([',', '*', '<', '>', '=', '!', '~', '^', ' '])
}

/// Strip the surrounding quotes a YAML scalar may carry, and any trailing
/// comment.
fn unquote(raw: &str) -> &str {
    let v = raw.trim();
    let v = match v.split_once(" #") {
        Some((before, _)) => before.trim(),
        None => v,
    };
    v.trim_matches(|c| c == '"' || c == '\'')
}

// ── mise.lock ─────────────────────────────────────────────────────────────────

/// Parse a `mise.lock` and add one suggestion per distinct download source.
///
/// Shape (see <https://mise.jdx.dev/dev-tools/mise-lock.html>):
///
/// ```toml
/// [[tools.node]]
/// version = "24.18.0"
/// backend = "core:node"
///
/// [tools.node."platforms.linux-x64"]
/// url = "https://nodejs.org/dist/v24.18.0/node-v24.18.0-linux-x64.tar.gz"
/// ```
fn collect_from_mise_lock(content: &str, acc: &mut Accumulator) {
    // `toml::from_str`, not `str::parse` — `FromStr for Value` parses a bare
    // TOML *value*, not a document, so it rejects `[[tools.x]]` outright.
    let Ok(value) = toml::from_str::<toml::Value>(content) else {
        return;
    };
    let Some(tools) = value.get("tools").and_then(|t| t.as_table()) else {
        return;
    };

    for (tool_name, entry) in tools {
        let source = format!("mise.lock: {tool_name}");
        // `[[tools.x]]` yields an array; tolerate a bare table too.
        let items: Vec<&toml::Value> = match entry {
            toml::Value::Array(arr) => arr.iter().collect(),
            other => vec![other],
        };
        for item in items {
            if let Some(table) = item.as_table() {
                collect_from_mise_lock_entry(table, tool_name, &source, acc);
            }
        }
    }
}

/// One `[[tools.x]]` entry: its per-platform URLs, or its backend if it has none.
fn collect_from_mise_lock_entry(
    table: &toml::Table,
    tool_name: &str,
    source: &str,
    acc: &mut Accumulator,
) {
    let mut saw_url = false;
    let subprocess = table
        .get("backend")
        .and_then(|b| b.as_str())
        .and_then(vfox_subprocess_note);
    // Every `platforms.*` sub-table carries a `url` for one platform.
    for sub in table.values() {
        let Some(url) = sub.get("url").and_then(|u| u.as_str()) else {
            continue;
        };
        saw_url = true;
        if let Some(mut reg) = registry_for_url(url, source) {
            if reg.note.is_none() {
                reg.note.clone_from(&subprocess);
            }
            acc.add(reg);
        }
    }
    if saw_url {
        return;
    }
    // No platform block (pure-source backends like `cargo:` / `pipx:`):
    // fall back to the backend prefix.
    let backend = table
        .get("backend")
        .and_then(|b| b.as_str())
        .unwrap_or(tool_name);
    if !acc.refuse(tool_name, backend) {
        for reg in registry_for_backend(backend, source) {
            acc.add(reg);
        }
    }
}

/// Map one concrete download URL onto a registry.
fn registry_for_url(url: &str, source: &str) -> Option<SuggestedRegistry> {
    let (host, path) = split_url(url)?;

    if let Some((_, registry_type, name)) = typed_host(host, path) {
        return Some(SuggestedRegistry {
            name: (*name).to_owned(),
            registry_type: (*registry_type).to_owned(),
            upstreams: Vec::new(),
            path_allow: Vec::new(),
            sources: vec![source.to_owned()],
            client_env: typed_client_env(registry_type),
            warm_packages: Vec::new(),
            revalidate: Vec::new(),
            max_artifact_size_bytes: None,
            note: None,
        });
    }

    if let Some(preset) = generic_preset(host, path) {
        // The preset upstream may include a path prefix (e.g. `/builds`);
        // the allowlist is relative to it, so the observed path is not reused.
        let owned = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect();
        return Some(SuggestedRegistry {
            name: preset.name.to_owned(),
            registry_type: "generic".to_owned(),
            upstreams: vec![preset.upstream.to_owned()],
            path_allow: owned(preset.path_allow),
            sources: vec![source.to_owned()],
            client_env: Vec::new(),
            warm_packages: Vec::new(),
            revalidate: owned(preset.revalidate),
            max_artifact_size_bytes: preset.max_artifact_size_bytes,
            note: None,
        });
    }

    Some(generic_from_unknown_url(host, path, source))
}

/// A `generic` mirror for a host we have no preset for. The upstream is the host
/// (plus the bucket segment on object-storage hosts) and the allowlist is the
/// single exact path observed — narrow by construction, since we have no basis
/// for guessing which parts of it are version components.
///
/// A bucket is one tenant's, so its allowlist is widened to the observed
/// file's directory (the checksum beside it) plus the version lists of
/// [`BUCKET_LIST_DOCUMENTS`] in that same bucket (RFC 0047 §6.1).
fn generic_from_unknown_url(host: &str, path: &str, source: &str) -> SuggestedRegistry {
    let is_bucket_host = BUCKET_HOSTS.contains(&host);
    let mut lists: Vec<String> = Vec::new();
    let (upstream, allow_path, name) = if is_bucket_host {
        match path.split_once('/') {
            Some((bucket, rest)) if !bucket.is_empty() => {
                let upstream = format!("https://{host}/{bucket}");
                // Same host and bucket, or not admitted (T6).
                let under = format!("{upstream}/");
                lists = BUCKET_LIST_DOCUMENTS
                    .iter()
                    .filter_map(|u| u.strip_prefix(&under))
                    .map(str::to_owned)
                    .collect();
                let dir = match rest.rsplit_once('/') {
                    Some((dir, _)) => format!("{dir}/*"),
                    None => "*".to_owned(),
                };
                (upstream, dir, slug_from_bucket(bucket))
            }
            _ => (
                format!("https://{host}"),
                path.to_owned(),
                slug_from_host(host),
            ),
        }
    } else {
        (
            format!("https://{host}"),
            path.to_owned(),
            slug_from_host(host),
        )
    };

    SuggestedRegistry {
        name,
        registry_type: "generic".to_owned(),
        upstreams: vec![upstream],
        path_allow: if allow_path.is_empty() {
            vec!["**".to_owned()]
        } else {
            std::iter::once(allow_path).chain(lists.clone()).collect()
        },
        sources: vec![source.to_owned()],
        client_env: Vec::new(),
        warm_packages: Vec::new(),
        revalidate: lists,
        max_artifact_size_bytes: None,
        note: Some(if is_bucket_host {
            "no preset for this bucket — path_allow lists the locked files' directories; \
             widen it (or re-run this command) when the pinned version changes"
                .to_owned()
        } else {
            "no preset for this host — path_allow lists the exact locked paths; \
             widen it (or re-run this command) when the pinned version changes"
                .to_owned()
        }),
    }
}

/// Map a mise backend string (`"aqua:cli/tool"`, `"cargo:ripgrep"`, `"core:rust"`)
/// onto a registry, for entries whose lock records no URL.
fn registry_for_backend(backend: &str, source: &str) -> Vec<SuggestedRegistry> {
    let typed = BACKEND_REGISTRIES
        .iter()
        .find(|(prefix, _, _)| backend.starts_with(prefix))
        .map(|(_, ty, name)| (*ty, *name))
        .or_else(|| predicate_registry(backend).map(|ty| (ty, ty)));
    if let Some((registry_type, name)) = typed {
        return vec![SuggestedRegistry {
            name: name.to_owned(),
            registry_type: registry_type.to_owned(),
            upstreams: Vec::new(),
            path_allow: Vec::new(),
            sources: vec![source.to_owned()],
            client_env: Vec::new(),
            warm_packages: Vec::new(),
            revalidate: Vec::new(),
            max_artifact_size_bytes: None,
            note: backend_suggestion_note(backend),
        }];
    }
    // `core:*` toolchains download from fixed hosts, so they resolve to what
    // the URL path would have produced.
    CORE_ROUTES
        .iter()
        .find(|(b, _)| *b == backend)
        .map_or(&[][..], |(_, urls)| urls)
        .iter()
        .filter_map(|url| registry_for_url(url, source))
        .collect()
}

// ── mise.toml ─────────────────────────────────────────────────────────────────

/// Parse a `mise.toml`'s `[tools]` table. Without a lock there are no URLs, so
/// only backend-prefixed keys and the handful of `core:` toolchains map
/// precisely; every other bare tool name resolves through mise's own registry,
/// which is overwhelmingly aqua/ubi (i.e. GitHub releases).
fn collect_from_mise_toml(content: &str, file: &str, acc: &mut Accumulator) {
    // `toml::from_str`, not `str::parse` — `FromStr for Value` parses a bare
    // TOML *value*, not a document, so it rejects `[[tools.x]]` outright.
    let Ok(value) = toml::from_str::<toml::Value>(content) else {
        return;
    };
    let Some(tools) = value.get("tools").and_then(|t| t.as_table()) else {
        return;
    };

    let mut saw_bare_tool = false;
    for tool_key in tools.keys() {
        let source = format!("{file}: {tool_key}");
        // As written, then a bare name as a `core:` toolchain.
        let routed = [tool_key.clone(), format!("core:{tool_key}")]
            .iter()
            .any(|backend| {
                let regs = registry_for_backend(backend, &source);
                let found = !regs.is_empty() || acc.refuse(tool_key, backend);
                regs.into_iter().for_each(|r| acc.add(r));
                found
            });
        if routed {
            continue;
        }
        saw_bare_tool = true;
        acc.add(SuggestedRegistry {
            name: "github".to_owned(),
            registry_type: "github".to_owned(),
            upstreams: Vec::new(),
            path_allow: Vec::new(),
            sources: vec![source],
            client_env: Vec::new(),
            warm_packages: Vec::new(),
            revalidate: Vec::new(),
            max_artifact_size_bytes: None,
            note: None,
        });
    }

    if saw_bare_tool {
        if let Some(gh) = acc.by_name.get_mut("github") {
            gh.note = Some(
                "unqualified tool names were assumed to resolve via aqua/ubi (GitHub releases); \
                 run `mise lock` and re-run this command for exact URLs"
                    .to_owned(),
            );
        }
    }
}

// ── Project manifests ─────────────────────────────────────────────────────────

/// Registry type reported by the manifest scanner → the config `type` value and
/// a suggested registry name.
fn manifest_registry(detected: &str) -> Option<(&'static str, &'static str)> {
    let mapped = match detected {
        "cargo" => ("cargo", "cargo"),
        "npm" => ("npm", "npm"),
        "pypi" => ("pypi", "pypi"),
        // The scanner's own label; the config type is `goproxy`.
        "gomodules" => ("goproxy", "go"),
        "maven" => ("maven", "maven"),
        "composer" => ("composer", "composer"),
        "rubygems" => ("rubygems", "rubygems"),
        "nuget" => ("nuget", "nuget"),
        "terraform" => ("terraform", "terraform"),
        "conda" => ("conda", "conda"),
        _ => return None,
    };
    Some(mapped)
}

fn collect_from_manifests(root: &Path, depth: usize, acc: &mut Accumulator) {
    // `scan_project_types` builds instruction text against a registry list;
    // only the detected type is used here, so an empty one never surfaces.
    let targets = crate::api::registry::RegistryTargets::new("", &[]);
    for det in scan_project_types(root, &targets, depth) {
        let Some((registry_type, name)) = manifest_registry(det.registry_type) else {
            continue;
        };
        let where_ = if det.relative_path.is_empty() {
            "manifest".to_owned()
        } else {
            format!("manifest in {}", det.relative_path)
        };
        acc.add(SuggestedRegistry {
            name: name.to_owned(),
            registry_type: registry_type.to_owned(),
            upstreams: Vec::new(),
            path_allow: Vec::new(),
            sources: vec![where_],
            client_env: Vec::new(),
            warm_packages: Vec::new(),
            revalidate: Vec::new(),
            max_artifact_size_bytes: None,
            note: None,
        });
    }
}

// ── Naming helpers ────────────────────────────────────────────────────────────

/// `"binaries.sonarsource.com"` → `"binaries-sonarsource"`. The TLD carries no
/// information and only lengthens every proxy URL.
fn slug_from_host(host: &str) -> String {
    let mut labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    if labels.first() == Some(&"www") {
        labels.remove(0);
    }
    if labels.len() > 1 {
        labels.pop();
    }
    let slug = labels.join("-");
    if slug.is_empty() {
        "upstream".to_owned()
    } else {
        slug
    }
}

/// `"claude-code-dist-86c565f3-f756-42ad-8dfa-d59b1c096819"` → `"claude-code-dist"`.
/// Bucket names commonly carry a random hex suffix that is pure noise in a URL
/// the user has to type; trailing hex-looking segments are dropped, but never
/// all of them.
fn slug_from_bucket(bucket: &str) -> String {
    let segments: Vec<&str> = bucket.split('-').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return "bucket".to_owned();
    }
    let mut end = segments.len();
    while end > 1 && looks_like_hex(segments[end - 1]) {
        end -= 1;
    }
    segments[..end].join("-")
}

fn looks_like_hex(s: &str) -> bool {
    s.len() >= 4 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Split `https://host/a/b` into `("host", "a/b")`. Only `http`/`https` URLs are
/// recognised; anything else (a `file:` path, a bare string) is skipped rather
/// than guessed at.
pub(crate) fn split_url(url: &str) -> Option<(&str, &str)> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let rest = rest.split('?').next().unwrap_or(rest);
    Some(match rest.split_once('/') {
        Some((host, path)) => (host, path),
        None => (rest, ""),
    })
}

// ── mise url_replacements ─────────────────────────────────────────────────────

/// The curated rules of a registry type: [`mise_wire::typed_rules`], read by
/// type name. An unknown type has none.
pub(crate) fn typed_url_replacements(
    registry_type: &str,
) -> &'static [(&'static str, &'static str)] {
    registry_type
        .parse::<RegistryKind>()
        .map_or(&[], mise_wire::typed_rules)
}

impl SuggestedRegistry {
    /// mise `[settings.url_replacements]` rules for this registry, as
    /// `(regex, replacement)` pairs with `{proxy}` already resolved.
    ///
    /// Returns empty for registry types mise never fetches itself — `pypi`,
    /// `goproxy`, `maven`, … are reached by a subprocess (`uv`, `go`, `mvn`),
    /// which reads its own config rather than mise's HTTP layer.
    pub fn mise_url_replacements(&self, server_url: &str) -> Vec<(String, String)> {
        let proxy = self.proxy_url(server_url);
        let resolve = |pattern: &str, replacement: &str| {
            (pattern.to_owned(), replacement.replace("{proxy}", &proxy))
        };

        if self.registry_type == "generic" {
            // A generic mirror is a 1:1 path passthrough, so its rule falls
            // straight out of the configured upstream.
            return self
                .upstreams
                .iter()
                .map(|u| {
                    resolve(
                        &format!("^{}/(.+)", regex_escape(u.trim_end_matches('/'))),
                        "{proxy}/$1",
                    )
                })
                .collect();
        }
        typed_url_replacements(&self.registry_type)
            .iter()
            .map(|(p, r)| resolve(p, r))
            .collect()
    }
}

pub(crate) use batlehub_core::services::mise_wire::regex_escape;

/// Quote a string as a TOML basic string.
///
/// This is where regex backslashes get doubled. TOML treats `\` as an escape
/// introducer, so a lone `\.` is a parse error — and mise reports it, keeps
/// going, and silently runs with the whole settings block dropped. Every regex
/// must therefore reach the file as `\\.`.
fn toml_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Render a mise `[settings.url_replacements]` block routing every rewritable
/// suggestion through the proxy.
///
/// `commented` prefixes every line with `# `, for committing into a shared
/// `mise.toml` where contributors without a BatleHub must not be broken.
/// The catch-all rule's target: everything no typed rule matched lands here,
/// is recorded, and answers `501` (RFC 0008 §4.4).
///
/// `$1` carries the host as the **first path segment**, so the recorder sees
/// the host without parsing the tail as a URL — which is what keeps the
/// sink's "opaque string for logging" rule cheap to hold.
pub const MISE_CATCH_ALL_PATTERN: &str = "^https://([^/]+)/(.*)";

/// The identity rule that has to precede the catch-all: this server's own
/// URLs, rewritten to themselves.
///
/// `None` when the server URL has no host to anchor on, in which case there
/// is nothing to protect and the catch-all stands alone.
pub fn mise_proxy_identity_rule(server_url: &str) -> Option<(String, String)> {
    let rest = server_url
        .trim_end_matches('/')
        .strip_prefix("https://")
        .or_else(|| server_url.trim_end_matches('/').strip_prefix("http://"))?;
    let host = rest.split('/').next().filter(|h| !h.is_empty())?;
    let scheme = if server_url.starts_with("http://") {
        "http"
    } else {
        "https"
    };
    Some((
        format!("^{scheme}://{}/(.*)", regex_escape(host)),
        format!("{scheme}://{host}/$1"),
    ))
}

pub fn mise_catch_all_rule(server_url: &str) -> (String, String) {
    (
        MISE_CATCH_ALL_PATTERN.to_owned(),
        format!(
            "{}/_air-gap/unmirrored/$1/$2",
            server_url.trim_end_matches('/')
        ),
    )
}

/// `catch_all` appends RFC 0008 §4.4's final rule, **last, always**.
///
/// mise applies `[settings.url_replacements]` in declaration order, first
/// match wins, and matching stops at the first hit (measured on mise 2026.8.6,
/// RFC 0008 decision 8). A catch-all declared before a typed rule swallows it,
/// and there is no fallback on failure — the rule that matched is the only URL
/// tried. So the ordering is a property of this generator rather than of the
/// operator's editing, which is why it is a parameter here and not a rule the
/// operator is told to add.
pub fn render_mise_toml(
    regs: &[SuggestedRegistry],
    server_url: &str,
    commented: bool,
    catch_all: bool,
) -> String {
    let mut body = String::new();
    body.push_str("[settings.url_replacements]\n");

    let skipped = push_mise_registry_rules(&mut body, regs, server_url);
    if catch_all {
        push_mise_catch_all(&mut body, server_url);
    }
    push_mise_skipped_note(&mut body, &skipped);
    push_mise_settings(&mut body, regs, server_url);

    let mut full = String::new();
    full.push_str("# Generated by `batlehub-cli registry suggest --mise`.\n");
    full.push_str("# Add to mise.toml, or ~/.config/mise/config.toml to apply everywhere.\n");
    if commented {
        full.push_str(
            "# Uncomment (strip one leading \"# \" per line) and set your BatleHub host.\n",
        );
    }
    full.push_str(&body);

    if !commented {
        return full;
    }
    comment_every_line(&full)
}

/// mise settings a rewrite rule needs beside it, as `(table, key, value, why)`
/// under `[settings]`, from the `generic` upstreams wired (RFC 0048 §6.2).
/// The one place such settings are decided, for `suggest` and `setup` alike.
pub(crate) fn mise_settings_for<'a>(
    generic_upstreams: impl IntoIterator<Item = &'a str>,
) -> Vec<(&'static str, &'static str, &'static str, &'static str)> {
    let mut out = Vec::new();
    // mise tries 17 community mirrors before ziglang.org (`zig.rs:151-170`),
    // so the rule on ziglang.org is reached only once they have all failed.
    if generic_upstreams
        .into_iter()
        .any(|u| u.trim_end_matches('/') == "https://ziglang.org")
    {
        out.push((
            "zig",
            "use_community_mirrors",
            "false",
            "community mirrors are tried before ziglang.org",
        ));
    }
    out
}

/// What a rule alone does not reach: the settings of [`mise_settings_for`],
/// mise's own node mirror setting, and rustup's variables under `[env]`.
fn push_mise_settings(body: &mut String, regs: &[SuggestedRegistry], server_url: &str) {
    let generic = regs
        .iter()
        .filter(|r| r.registry_type == "generic")
        .flat_map(|r| r.upstreams.iter().map(String::as_str));
    for (table, key, value, why) in mise_settings_for(generic) {
        body.push_str(&format!(
            "\n[settings.{table}]\n{key} = {value}   # {why}\n"
        ));
    }
    if let Some(node) = regs.iter().find(|r| r.registry_type == "nodedist") {
        // mise `Url::join`s onto it: without the slash the last segment is lost.
        let url = format!("{}/", node.proxy_url(server_url));
        body.push_str(&format!(
            "\n[settings.node]\nmirror_url = {}\n",
            toml_quote(&url)
        ));
    }
    let env: Vec<_> = regs
        .iter()
        .filter(|r| r.registry_type == "rustup")
        .flat_map(|r| r.resolved_env(server_url))
        .collect();
    if !env.is_empty() {
        body.push_str("\n# rustup is a subprocess: it reads these, not url_replacements.\n[env]\n");
        for (k, v) in env {
            body.push_str(&format!("{k} = {}\n", toml_quote(&v)));
        }
    }
}

/// One block of rules per registry mise can rewrite for. Returns the registry
/// types it could not, for the trailing note.
fn push_mise_registry_rules<'a>(
    body: &mut String,
    regs: &'a [SuggestedRegistry],
    server_url: &str,
) -> Vec<&'a str> {
    let mut any = false;
    let mut skipped: Vec<&str> = Vec::new();
    for reg in regs {
        let rules = reg.mise_url_replacements(server_url);
        // rustup is a subprocess too, but mise hands it the `[env]` below.
        if rules.is_empty() && reg.registry_type != "rustup" {
            if !skipped.contains(&reg.registry_type.as_str()) {
                skipped.push(&reg.registry_type);
            }
            continue;
        }
        any = true;
        body.push_str(&format!("\n# {} ({})\n", reg.name, reg.registry_type));
        for (pattern, replacement) in rules {
            push_replacement(body, &pattern, &replacement);
        }
    }
    if !any {
        body.push_str("# (nothing mise downloads itself — see the note below)\n");
    }
    skipped
}

/// One `"regex:…" = "…"` line.
pub(crate) fn push_replacement(body: &mut String, pattern: &str, replacement: &str) {
    body.push_str(&format!(
        "{} = {}\n",
        toml_quote(&format!("regex:{pattern}")),
        toml_quote(replacement)
    ));
}

/// A commented paragraph, wrapped.
fn push_note(body: &mut String, text: &str) {
    for line in wrap_comment(text) {
        body.push_str(&format!("# {line}\n"));
    }
}

/// The two rules that close RFC 0008 §4.4's coverage, in the order mise reads
/// them.
///
/// The proxy's own host comes first. The catch-all matches *any* https URL,
/// BatleHub's included, so an operator who has already pointed a backend at the
/// proxy — `NODEJS_ORG_MIRROR`, a `[settings]` entry, a hand-written rule below
/// — would have that URL rewritten into the `501` sink and lose a registry that
/// was working. Matching stops at the first hit, so one identity rule ahead of
/// the catch-all is the whole fix (RFC 0008 §13, *Coverage, stated*).
fn push_mise_catch_all(body: &mut String, server_url: &str) {
    if let Some((pattern, replacement)) = mise_proxy_identity_rule(server_url) {
        body.push('\n');
        push_note(
            body,
            "This server's own URLs are left alone. The catch-all below matches every \
             https host, so without this line a backend already pointed at BatleHub \
             would be rewritten into the unmirrored sink.",
        );
        push_replacement(body, &pattern, &replacement);
    }
    let (pattern, replacement) = mise_catch_all_rule(server_url);
    body.push('\n');
    push_note(
        body,
        "Air gap (RFC 0008): anything no rule above matched lands here. Nothing is \
         fetched — the proxy answers 501, names the host and records it, so an \
         unmirrored host is a line in the console instead of a connect timeout. \
         This rule must stay last: mise stops at the first match.",
    );
    push_replacement(body, &pattern, &replacement);
}

/// The trailing note naming the registry types `url_replacements` cannot cover.
fn push_mise_skipped_note(body: &mut String, skipped: &[&str]) {
    if skipped.is_empty() {
        return;
    }
    body.push('\n');
    for line in wrap_comment(&format!(
        "Not rewritable here: {}. mise shells out to the ecosystem's own tool \
         for these, so point that tool at the proxy instead (.cargo/config.toml, \
         pip.conf / UV_INDEX_URL, GOPROXY, .npmrc, …) — see \
         `batlehub-cli registry suggest --client-env`.",
        skipped.join(", ")
    )) {
        body.push_str(&format!("# {line}\n"));
    }
}

/// Prefix *every* line, header comments included. Stripping one "# " must
/// yield a valid file, so the header has to survive as `# …` rather than
/// become a bare sentence the TOML parser chokes on.
fn comment_every_line(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if !line.is_empty() {
            out.push_str("# ");
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

// ── Rendering ─────────────────────────────────────────────────────────────────

/// Render the suggestions as `[[registries]]` blocks ready to paste into
/// `config.toml`.
pub fn render_toml(regs: &[SuggestedRegistry], unroutable: &[String]) -> String {
    let mut out = String::new();
    out.push_str("# Generated by `batlehub-cli registry suggest`.\n");
    out.push_str("# Review before applying — names are suggestions, and RBAC below is a\n");
    out.push_str("# permissive starting point.\n");
    for reg in regs {
        out.push('\n');
        push_registry_block(&mut out, reg);
    }
    // Where the operator is already reading, not only in `mise plan`.
    if !unroutable.is_empty() {
        out.push_str("\n# Not routable through BatleHub:\n");
        for line in unroutable {
            for (i, l) in wrap_comment(line).iter().enumerate() {
                out.push_str(if i == 0 { "#   " } else { "#     " });
                out.push_str(l);
                out.push('\n');
            }
        }
    }
    out
}

/// One `[[registries]]` block: its provenance comments, then its keys.
fn push_registry_block(out: &mut String, reg: &SuggestedRegistry) {
    for src in &reg.sources {
        out.push_str(&format!("# from {src}\n"));
    }
    if let Some(note) = &reg.note {
        for line in wrap_comment(note) {
            out.push_str(&format!("# {line}\n"));
        }
    }
    out.push_str("[[registries]]\n");
    out.push_str(&format!("type       = \"{}\"\n", reg.registry_type));
    out.push_str(&format!("name       = \"{}\"\n", reg.name));
    if reg.upstreams.is_empty() {
        out.push_str("# upstreams omitted — the adapter's default upstream applies\n");
    } else {
        out.push_str(&format!(
            "upstreams  = [{}]\n",
            reg.upstreams
                .iter()
                .map(|u| format!("\"{u}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for (key, list) in [
        ("path_allow", &reg.path_allow),
        ("revalidate", &reg.revalidate),
    ] {
        if list.is_empty() {
            continue;
        }
        if key == "revalidate" {
            out.push_str("# re-read upstream on every request, served stale if it fails\n");
        }
        out.push_str(&format!("{key:<10} = [\n"));
        for p in list {
            out.push_str(&format!("  \"{p}\",\n"));
        }
        out.push_str("]\n");
    }
    if let Some(limit) = reg.max_artifact_size_bytes {
        out.push_str(&format!(
            "# Above [limits] max_artifact_size_bytes (500 MiB by default); this\n\
             # registry alone is raised.\n\
             max_artifact_size_bytes = {limit}\n"
        ));
    }
    out.push_str("\n[registries.rbac]\n");
    // The toolchain kinds read their listings with a verb of their own.
    if matches!(reg.registry_type.as_str(), "nodedist" | "sdkman") {
        out.push_str("anonymous = [\"releases:read\", \"releases:list\"]\n");
    } else {
        out.push_str("anonymous = [\"releases:read\"]\n");
    }
    if !reg.warm_packages.is_empty() {
        out.push_str("\n# Pre-fetched on startup, for the platform this server runs on; list\n");
        out.push_str("# others under warm_platforms to warm them too (RFC 0010 §6.9).\n");
        out.push_str("[registries.cache]\n");
        out.push_str(&format!(
            "warm_packages = [{}]\n",
            reg.warm_packages
                .iter()
                .map(|p| format!("\"{p}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
}

/// Render the client-side environment variables for the suggestions that have
/// any, resolved against `server_url`.
pub fn render_client_env(regs: &[SuggestedRegistry], server_url: &str) -> String {
    let mut out = String::new();
    let mut any = false;
    for reg in regs {
        let env = reg.resolved_env(server_url);
        if env.is_empty() {
            continue;
        }
        any = true;
        out.push_str(&format!("# {} ({})\n", reg.name, reg.registry_type));
        for (k, v) in env {
            out.push_str(&format!("export {k}=\"{v}\"\n"));
        }
        out.push('\n');
    }
    if !any {
        out.push_str(
            "# No toolchain in this project has a dedicated mirror variable.\n\
             # Point clients at the proxy URLs directly, e.g.\n",
        );
        for reg in regs.iter().filter(|r| r.registry_type == "generic") {
            out.push_str(&format!("#   {}/<path>\n", reg.proxy_url(server_url)));
        }
    }
    out
}

/// Break a long note into ~76-column comment lines.
fn wrap_comment(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if !current.is_empty() && current.len() + 1 + word.len() > 76 {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn names(regs: &[SuggestedRegistry]) -> Vec<&str> {
        regs.iter().map(|r| r.name.as_str()).collect()
    }

    fn find<'a>(regs: &'a [SuggestedRegistry], name: &str) -> &'a SuggestedRegistry {
        regs.iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("no suggestion named {name} in {:?}", names(regs)))
    }

    #[test]
    fn split_url_splits_host_and_path() {
        assert_eq!(
            split_url("https://nodejs.org/dist/v1/x.tar.gz"),
            Some(("nodejs.org", "dist/v1/x.tar.gz"))
        );
        assert_eq!(split_url("https://example.com"), Some(("example.com", "")));
        assert_eq!(split_url("ftp://example.com/x"), None);
    }

    #[test]
    fn slug_from_host_drops_tld_and_www() {
        assert_eq!(
            slug_from_host("binaries.sonarsource.com"),
            "binaries-sonarsource"
        );
        assert_eq!(slug_from_host("www.example.com"), "example");
        assert_eq!(slug_from_host("localhost"), "localhost");
    }

    #[test]
    fn slug_from_bucket_drops_trailing_hex_noise() {
        assert_eq!(
            slug_from_bucket("claude-code-dist-86c565f3-f756-42ad-8dfa-d59b1c096819"),
            "claude-code-dist"
        );
        assert_eq!(slug_from_bucket("my-releases"), "my-releases");
        // Never strips everything, even when every segment looks hex.
        assert_eq!(slug_from_bucket("dead-beef"), "dead");
    }

    #[test]
    fn github_release_url_maps_to_a_typed_github_registry() {
        let reg = registry_for_url(
            "https://github.com/cocogitto/cocogitto/releases/download/7.0.0/x.tar.gz",
            "mise.lock: cocogitto",
        )
        .unwrap();
        assert_eq!(reg.registry_type, "github");
        // Typed adapters carry their own default upstream.
        assert!(reg.upstreams.is_empty());
        assert!(reg.path_allow.is_empty());
    }

    #[test]
    fn preset_host_maps_to_a_generic_mirror_with_version_agnostic_globs() {
        let reg = registry_for_url(
            "https://ziglang.org/download/0.15.2/zig-x86_64-linux-0.15.2.tar.xz",
            "mise.lock: zig",
        )
        .unwrap();
        assert_eq!(reg.registry_type, "generic");
        assert_eq!(reg.name, "zig-dl");
        assert_eq!(reg.upstreams, vec!["https://ziglang.org"]);
        assert_eq!(reg.path_allow, vec!["builds/**", "download/**"]);
        assert_eq!(
            reg.revalidate,
            vec!["download/community-mirrors.txt", "download/index.json"]
        );
        assert!(reg.client_env.is_empty());
    }

    /// RFC 0048 §6.2: Node and Rust have typed kinds, so their hosts suggest
    /// them, with the client variables the deleted presets carried.
    #[test]
    fn node_and_rust_hosts_suggest_their_typed_kind() {
        let node = registry_for_url(
            "https://nodejs.org/dist/v24.18.0/node-v24.18.0-linux-x64.tar.gz",
            "s",
        )
        .unwrap();
        assert_eq!(
            (node.name.as_str(), node.registry_type.as_str()),
            ("node", "nodedist")
        );
        assert!(node.upstreams.is_empty() && node.path_allow.is_empty());
        assert_eq!(
            node.resolved_env("https://hub.example.com")[3],
            (
                "NODEJS_ORG_MIRROR".to_owned(),
                "https://hub.example.com/proxy/node/nodedist".to_owned()
            )
        );
        let rust = registry_for_url(
            "https://static.rust-lang.org/dist/channel-rust-1.98.1.toml",
            "s",
        )
        .unwrap();
        assert_eq!(rust.registry_type, "rustup");
        assert_eq!(
            rust.resolved_env("https://hub.example.com"),
            [
                (
                    "RUSTUP_DIST_SERVER".to_owned(),
                    "https://hub.example.com/proxy/rust/rustup".to_owned()
                ),
                (
                    "RUSTUP_UPDATE_ROOT".to_owned(),
                    "https://hub.example.com/proxy/rust/rustup/rustup".to_owned()
                ),
            ]
        );
        // The typed rule, and a plan path that agrees with it.
        assert_eq!(
            node.mise_url_replacements("https://hub.example.com"),
            [(
                r"^https://nodejs\.org/dist/(.+)".to_owned(),
                "https://hub.example.com/proxy/node/nodedist/$1".to_owned()
            )]
        );
        assert_eq!(
            proxy_path_for(
                "https://nodejs.org/dist/v24.18.0/node-v24.18.0-linux-x64.tar.gz",
                "node",
                "nodedist"
            )
            .as_deref(),
            Some("/proxy/node/nodedist/v24.18.0/node-v24.18.0-linux-x64.tar.gz")
        );
        // Either kind plans these hosts, the typed one first.
        assert_eq!(
            registry_kinds_for_host("nodejs.org", "dist/v1/x"),
            ["nodedist", "generic"]
        );
        assert_eq!(
            registry_kinds_for_host("static.rust-lang.org", "dist/x"),
            ["rustup", "generic"]
        );
    }

    /// RFC 0048 §14: every `core:` toolchain of mise 2026.11.3
    /// (`src/plugins/core/`) has one stated route; a new one is a red test.
    #[test]
    fn every_core_toolchain_has_a_stated_route() {
        let route = |tool: &str| {
            let backend = format!("core:{tool}");
            let regs: Vec<String> = registry_for_backend(&backend, "s")
                .into_iter()
                .map(|r| format!("{}:{}", r.registry_type, r.name))
                .collect();
            (regs, core_refusal(&backend).is_some())
        };
        let table: [(&str, &[&str], bool); 13] = [
            ("node", &["nodedist:node"], false),
            ("rust", &["rustup:rust"], false),
            ("go", &["godist:godist"], false),
            ("deno", &["generic:deno-dl"], false),
            ("zig", &["generic:zig-dl"], false),
            ("elixir", &["generic:hex-builds"], false),
            ("erlang", &["generic:hex-builds"], false),
            ("swift", &["generic:swift-dl", "generic:swift-api"], false),
            // Through their locked GitHub URLs.
            ("python", &[], false),
            ("bun", &[], false),
            ("ruby", &[], false),
            // The `mise-java` kind is phase 2.
            ("java", &[], false),
            ("dotnet", &[], true),
        ];
        for (tool, regs, refused) in table {
            assert_eq!(
                route(tool),
                (regs.iter().map(|s| (*s).to_owned()).collect(), refused),
                "{tool}"
            );
        }
        let reason = unsupported_backend_reason("core:dotnet").unwrap();
        assert!(
            reason.starts_with("core:dotnet: mise runs dotnet-install.sh"),
            "{reason}"
        );
        // No URL for a tool whose precompiled install always records one.
        assert!(unsupported_backend_reason("core:python")
            .unwrap()
            .contains("python.compile"));
        assert_eq!(unsupported_backend_reason("core:swift"), None);
        // A macOS erlang lock names GitHub by URL.
        let mac = registry_for_url(
            "https://github.com/erlef/otp_builds/releases/download/OTP-28.3/otp-aarch64-apple-darwin.tar.gz",
            "s",
        )
        .unwrap();
        assert_eq!(mac.registry_type, "github");
    }

    /// Each preset admits every path mise 2026.11.3 requests (probed
    /// 2026-11-14) and refuses the host root and other products (RFC 0047
    /// §7.1 T2), under today's `*`-crosses-`/` globs and RFC 0047's
    /// `require_literal_separator` alike.
    #[test]
    fn presets_admit_what_mise_fetches_and_nothing_else() {
        let corpus: &[(&str, &[&str], &[&str])] = &[
            (
                "ziglang.org",
                &[
                    "download/0.15.2/zig-x86_64-linux-0.15.2.tar.xz",
                    "download/0.15.2/zig-x86_64-linux-0.15.2.tar.xz.minisig",
                    "download/index.json",
                    "download/community-mirrors.txt",
                    "builds/zig-x86_64-linux-0.16.0-dev.1+abc.tar.xz",
                ],
                &["", "index.html", "learn/overview/"],
            ),
            (
                "builds.hex.pm",
                &[
                    "elixir/v1.18.4-otp-27.zip",
                    "elixir/builds.txt",
                    "otp/amd64/ubuntu-24.04/OTP-28.3.tar.gz",
                    "otp/amd64/ubuntu-24.04/builds.txt",
                ],
                &["", "installs/hex-1.csv", "builds.txt"],
            ),
            (
                "dl.deno.land",
                &[
                    "release/v2.5.6/deno-x86_64-unknown-linux-gnu.zip",
                    "release/v2.5.6/deno-x86_64-unknown-linux-gnu.zip.sha256sum",
                ],
                &["", "release-latest.txt", "canary/x.zip"],
            ),
            (
                "download.swift.org",
                &[
                    "swift-6.2-release/ubuntu2404/swift-6.2-RELEASE/swift-6.2-RELEASE-ubuntu24.04.tar.gz",
                    "swift-6.4.2-release/ubuntu2404-aarch64/swift-6.4.2-RELEASE/swift-6.4.2-RELEASE-ubuntu24.04-aarch64.tar.gz.sig",
                ],
                &["", "development/ubuntu2404/x.tar.gz", "swiftly/linux/swiftly.tar.gz"],
            ),
            ("www.swift.org", &["releases.json"], &["", "index.html", "x/releases.json"]),
            (
                "downloads.python.org",
                &[
                    "versions.json",
                    "pypy3.11-v7.3.20-linux64.tar.bz2",
                    "pypy3.12-v7.3.21-aarch64.tar.bz2",
                    "pypy2.7-v7.3.20-win64.zip",
                ],
                &["", "index.html", "ftp/python/3.13.0/Python-3.13.0.tgz"],
            ),
            // RFC 0047 phase 2: paths read from mise 2026.11.3's registry, its
            // aqua registry and its embedded vfox plugins, probed 2026-11-14.
            (
                "releases.hashicorp.com",
                &[
                    "terraform/1.9.8/terraform_1.9.8_linux_amd64.zip",
                    "terraform/1.9.8/terraform_1.9.8_SHA256SUMS",
                    "terraform-ls/0.36.3/terraform-ls_0.36.3_linux_amd64.zip",
                    "consul/1.20.1/consul_1.20.1_darwin_arm64.zip",
                    "boundary/0.18.0/boundary_0.18.0_linux_amd64.zip",
                    "nomad/1.9.3/nomad_1.9.3_linux_amd64.zip",
                    "packer/1.11.2/packer_1.11.2_linux_amd64.zip",
                    "vault/1.18.2/vault_1.18.2_linux_amd64.zip",
                    "waypoint/0.11.4/waypoint_0.11.4_linux_amd64.zip",
                    "sentinel/0.30.0/sentinel_0.30.0_linux_amd64.zip",
                    "sentinel/index.json",
                    "tfc-agent/index.json",
                    "nomad-pack/index.json",
                ],
                &[
                    "",
                    "index.json",
                    "vagrant/2.4.3/vagrant_2.4.3_linux_amd64.zip",
                    "terraform-provider-aws/5.80.0/x.zip",
                    "consul-template/0.39.1/x.zip",
                ],
            ),
            (
                "api.releases.hashicorp.com",
                &["sentinel", "tfc-agent", "nomad-pack"],
                &["", "terraform", "sentinel/0.30.0", "vault"],
            ),
            (
                "dl.k8s.io",
                &[
                    "v1.31.0/bin/linux/amd64/kubectl",
                    "v1.31.0/bin/linux/amd64/kubectl.sha256",
                    "v1.31.0/bin/darwin/arm64/kubectl.sig",
                    "v1.31.0/bin/linux/amd64/kubectl-convert",
                    "v1.31.0/bin/windows/amd64/kubectl.exe",
                    "release/stable.txt",
                ],
                &[
                    "",
                    "v1.31.0/bin/linux/amd64/kubelet",
                    "v1.31.0/kubernetes-server-linux-amd64.tar.gz",
                    "release/stable-1.31.txt",
                ],
            ),
            (
                "archive.apache.org",
                &[
                    "ant/binaries/apache-ant-1.10.15-bin.tar.gz",
                    "ant/binaries/apache-ant-1.10.15-bin.tar.gz.sha512",
                    "groovy/4.0.24/distribution/apache-groovy-binary-4.0.24.zip",
                    "maven/maven-3/3.9.9/binaries/apache-maven-3.9.9-bin.tar.gz.sha512",
                    "spark/spark-3.5.3/spark-3.5.3-bin-hadoop3.tgz",
                    "tomcat/tomcat-10/v10.1.34/bin/apache-tomcat-10.1.34.tar.gz",
                ],
                &["", "httpd/httpd-2.4.62.tar.gz", "hadoop/common/x.tgz", "antlr/x"],
            ),
            (
                "mirror.openshift.com",
                &[
                    "ocp/4.17.10/openshift-client-linux.tar.gz",
                    "ocp/4.17.10/openshift-install-linux-4.17.10.tar.gz",
                    "ocp/4.17.10/sha256sum.txt",
                ],
                &[
                    "",
                    "ocp/4.17.10/oc-mirror.tar.gz",
                    "ocp-dev-preview/4.18.0-ec.4/openshift-client-linux.tar.gz",
                    "odo/v3.16.1/odo-linux-amd64",
                ],
            ),
            (
                "julialang-s3.julialang.org",
                &[
                    "versions.json",
                    "linux/x64/1.11/julia-1.11.2-linux-x86_64.tar.gz",
                    "mac/aarch64/1.11/julia-1.11.2-macaarch64.tar.gz",
                    "winnt/x64/1.11/julia-1.11.2-win64.zip",
                ],
                &["", "checksums/julia-1.11.2.sha256", "freebsd/x64/1.11/x.tar.gz"],
            ),
            (
                "download.docker.com",
                &[
                    "linux/static/stable/x86_64/docker-27.3.1.tgz",
                    "mac/static/stable/aarch64/docker-27.3.1.tgz",
                    "win/static/stable/x86_64/docker-27.3.1.zip",
                ],
                &[
                    "",
                    "linux/ubuntu/dists/noble/stable/binary-amd64/Packages",
                    "linux/static/test/x86_64/docker-28.0.0-rc.1.tgz",
                    "mac/beta/Docker.dmg",
                ],
            ),
            (
                "dl.google.com/android/",
                &[
                    "repository/repository2-3.xml",
                    "repository/commandlinetools-linux-11479570_latest.zip",
                    "maven2/com/android/tools/build/group-index.xml",
                    "maven2/com/android/tools/build/aapt2/8.7.3-12006047/aapt2-8.7.3-12006047-linux.jar",
                    "cli/1.1.16919734/linux_x86_64/android",
                    "cli/1.1.16919734/linux_x86_64/android.sha256",
                    "cli/latest/debian/dists/stable/Release",
                    "cli/latest/debian/dists/stable/main/binary-amd64/Packages",
                ],
                &[
                    "",
                    "repository/platform-tools-latest-linux.zip",
                    "maven2/com/android/tools/build/gradle/8.7.3/gradle-8.7.3.jar",
                    "studio/ide-zips/2024.2.1.11/android-studio.tar.gz",
                ],
            ),
        ];
        for (key, admitted, refused) in corpus {
            // A key with a `/` is a host plus a path prefix, as in `TYPED_HOSTS`.
            let (host, path) = key.split_once('/').unwrap_or((key, ""));
            let preset = generic_preset(host, path).unwrap();
            for literal in [false, true] {
                let opts = glob::MatchOptions {
                    require_literal_separator: literal,
                    ..glob::MatchOptions::new()
                };
                let allows = |path: &str| {
                    preset
                        .path_allow
                        .iter()
                        .any(|g| glob::Pattern::new(g).unwrap().matches_with(path, opts))
                };
                for path in *admitted {
                    assert!(allows(path), "{host}: {path} must be admitted");
                }
                for path in *refused {
                    assert!(!allows(path), "{host}: {path} must be refused");
                }
            }
            // A revalidated path must be one the preset serves.
            for r in preset.revalidate {
                assert!(
                    admitted
                        .iter()
                        .any(|p| glob::Pattern::new(r).unwrap().matches_with(
                            p,
                            glob::MatchOptions {
                                require_literal_separator: true,
                                ..glob::MatchOptions::new()
                            }
                        )),
                    "{host}: {r}"
                );
            }
        }
    }

    #[test]
    fn bucket_host_puts_the_bucket_in_the_upstream_not_the_allowlist() {
        // This is the case path_allow exists for: without the bucket in the
        // upstream, the mirror would relay every public GCS bucket.
        let reg = registry_for_url(
            "https://storage.googleapis.com/claude-code-dist-86c565f3-f756-42ad-8dfa-d59b1c096819/claude-code-releases/2.1.217/linux-x64/claude",
            "mise.lock: claude",
        )
        .unwrap();
        assert_eq!(reg.registry_type, "generic");
        assert_eq!(reg.name, "claude-code-dist");
        assert_eq!(
            reg.upstreams,
            vec![
                "https://storage.googleapis.com/claude-code-dist-86c565f3-f756-42ad-8dfa-d59b1c096819"
            ]
        );
        // RFC 0047 §6.1: the file's directory, plus the version list mise
        // reads from the same bucket, which is revalidated.
        assert_eq!(
            reg.path_allow,
            vec![
                "claude-code-releases/2.1.217/linux-x64/*",
                "claude-code-releases/latest"
            ]
        );
        assert_eq!(reg.revalidate, ["claude-code-releases/latest"]);
        assert!(
            reg.note.is_some(),
            "unknown hosts must carry the widen-me caveat"
        );
    }

    /// RFC 0047 phase 2: every new preset, and a widened bucket mirror, is a
    /// block the server loads — each `revalidate` inside its `path_allow`
    /// under the server's own matching.
    #[test]
    fn phase_two_presets_render_a_config_the_server_loads() {
        let urls = [
            "https://releases.hashicorp.com/terraform/1.9.8/terraform_1.9.8_linux_amd64.zip",
            "https://api.releases.hashicorp.com/v1/releases/sentinel?limit=20",
            "https://dl.k8s.io/v1.31.0/bin/linux/amd64/kubectl",
            "https://archive.apache.org/dist/ant/binaries/apache-ant-1.10.15-bin.tar.gz",
            "https://mirror.openshift.com/pub/openshift-v4/clients/ocp/4.17.10/openshift-client-linux.tar.gz",
            "https://julialang-s3.julialang.org/bin/linux/x64/1.11/julia-1.11.2-linux-x86_64.tar.gz",
            "https://download.docker.com/linux/static/stable/x86_64/docker-27.3.1.tgz",
            "https://dl.google.com/android/repository/commandlinetools-linux-11479570_latest.zip",
            "https://storage.googleapis.com/flutter_infra_release/releases/stable/linux/flutter_linux_3.24.0-stable.tar.xz",
        ];
        let regs: Vec<_> = urls
            .iter()
            .map(|u| registry_for_url(u, "s").unwrap())
            .collect();
        let names: Vec<&str> = regs.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "hashicorp-releases",
                "hashicorp-api",
                "k8s-dl",
                "apache-archive",
                "openshift-mirror",
                "julia-dist",
                "docker-dl",
                "android-dl",
                "flutter_infra_release"
            ]
        );
        let config = format!(
            "[server]\nhost = \"127.0.0.1\"\nport = 8080\n[database]\ntype = \"postgresql\"\n\
             url = \"postgresql://u:p@localhost/db\"\n[storage]\ntype = \"filesystem\"\n\
             path = \"./tmp\"\n{}",
            render_toml(&regs, &[])
        );
        batlehub_config::load_from_str(&config).unwrap_or_else(|e| panic!("{e:#}\n{config}"));
    }

    /// A bucket admits only the version lists in its own bucket on its own
    /// host (RFC 0047 §7.1 T6): flutter's lists never reach skaffold's mirror.
    #[test]
    fn a_bucket_admits_only_its_own_version_lists() {
        let flutter = registry_for_url(
            "https://storage.googleapis.com/flutter_infra_release/releases/stable/linux/flutter_linux_3.24.0-stable.tar.xz",
            "s",
        )
        .unwrap();
        assert_eq!(
            flutter.upstreams,
            ["https://storage.googleapis.com/flutter_infra_release"]
        );
        assert_eq!(
            flutter.path_allow,
            [
                "releases/stable/linux/*",
                "releases/releases_linux.json",
                "releases/releases_macos.json",
                "releases/releases_windows.json",
            ]
        );
        let skaffold = registry_for_url(
            "https://storage.googleapis.com/skaffold/releases/v2.13.2/skaffold-linux-amd64",
            "s",
        )
        .unwrap();
        assert_eq!(skaffold.path_allow, ["releases/v2.13.2/*"]);
        assert!(skaffold.revalidate.is_empty());
        // A file at the bucket's root: the root's files, nothing below.
        let root =
            registry_for_url("https://storage.googleapis.com/some-bucket/tool.tgz", "s").unwrap();
        assert_eq!(root.path_allow, ["*"]);
        // Not a bucket host: still the one exact path.
        let plain = registry_for_url("https://tool.example.com/a/b/tool.tgz", "s").unwrap();
        assert_eq!(plain.path_allow, ["a/b/tool.tgz"]);
    }

    #[test]
    fn urlless_backends_map_by_prefix() {
        let reg = registry_for_backend("cargo:cargo-audit", "mise.lock: cargo-audit")
            .pop()
            .unwrap();
        assert_eq!(reg.registry_type, "cargo");
        let reg = registry_for_backend("pipx:semgrep", "mise.lock: semgrep")
            .pop()
            .unwrap();
        assert_eq!(reg.registry_type, "pypi");
        let reg = registry_for_backend("aqua:owner/tool", "mise.lock: tool")
            .pop()
            .unwrap();
        assert_eq!(reg.registry_type, "github");
        let reg = registry_for_backend("conda:ffmpeg", "mise.lock: ffmpeg")
            .pop()
            .unwrap();
        assert_eq!(reg.registry_type, "conda");
        // core:rust ships no platform URL in the lock, so the backend must carry it.
        let reg = registry_for_backend("core:rust", "mise.lock: rust")
            .pop()
            .unwrap();
        assert_eq!(
            (reg.name.as_str(), reg.registry_type.as_str()),
            ("rust", "rustup")
        );
        // core:go has its typed kind (RFC 0048 phase 5), so no generic mirror.
        let reg = registry_for_backend("core:go", "mise.lock: go")
            .pop()
            .unwrap();
        assert_eq!(reg.registry_type, "godist");
        assert!(reg.upstreams.is_empty());
        let reg =
            registry_for_url("https://dl.google.com/go/go1.25.4.linux-amd64.tar.gz", "s").unwrap();
        assert_eq!(reg.registry_type, "godist");
        // `/android/` on the same host is the `android-dl` preset (RFC 0047
        // §15 Q3), and the plan routes it through a `generic` registry.
        let android =
            "https://dl.google.com/android/repository/commandlinetools-linux-11479570_latest.zip";
        let reg = registry_for_url(android, "s").unwrap();
        assert_eq!(
            (reg.name.as_str(), reg.registry_type.as_str()),
            ("android-dl", "generic")
        );
        assert_eq!(reg.upstreams, ["https://dl.google.com/android"]);
        let (host, path) = split_url(android).unwrap();
        assert_eq!(registry_kinds_for_host(host, path), ["generic"]);
        assert_eq!(
            proxy_path_for(android, "android-dl", "generic").as_deref(),
            Some("/proxy/android-dl/generic/repository/commandlinetools-linux-11479570_latest.zip")
        );
        // The rest of the host is neither: the narrow generic of an unknown host.
        let other = "https://dl.google.com/linux/chrome/deb/dists/stable/Release";
        assert_eq!(
            registry_for_url(other, "s").unwrap().path_allow,
            ["linux/chrome/deb/dists/stable/Release"]
        );
        let (host, path) = split_url(other).unwrap();
        assert!(registry_kinds_for_host(host, path).is_empty());
        assert!(registry_for_backend("core:unknown-thing", "x").is_empty());
    }

    // ── requirements.yml and galaxy.yml (RFC 0031 §6.9) ──────────────────────

    #[test]
    fn a_requirements_yml_maps_to_a_galaxy_registry_and_warms_its_pins() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("requirements.yml"),
            "---\n\
             collections:\n\
             \x20 # the one we pin\n\
             \x20 - name: community.general\n\
             \x20   version: \"13.4.0\"\n\
             \x20 - name: community.docker\n\
             \x20   version: \">=4.0.0\"\n\
             \x20 - ansible.posix\n\
             roles:\n\
             \x20 - name: geerlingguy.docker\n",
        )
        .unwrap();
        let regs = suggest_registries(dir.path(), 0);
        let g = find(&regs, "galaxy");
        assert_eq!(g.registry_type, "galaxy");
        assert_eq!(g.sources, ["requirements.yml"]);
        // Only the exact pin is warmed: a range names no release, and a bare
        // entry names no version at all.
        assert_eq!(g.warm_packages, ["community.general@13.4.0"]);
        assert_eq!(
            g.resolved_env("https://hub.example.com/"),
            [
                (
                    "ANSIBLE_GALAXY_SERVER_LIST".to_owned(),
                    "batlehub".to_owned()
                ),
                (
                    "ANSIBLE_GALAXY_SERVER_BATLEHUB_URL".to_owned(),
                    "https://hub.example.com/proxy/galaxy/galaxy/api/".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn a_collections_requirements_yml_is_read_too() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("collections")).unwrap();
        std::fs::write(
            dir.path().join("collections").join("requirements.yml"),
            "collections:\n  - name: acme.util\n    version: 1.0.0\n",
        )
        .unwrap();
        let regs = suggest_registries(dir.path(), 0);
        let g = find(&regs, "galaxy");
        assert_eq!(g.sources, ["collections/requirements.yml"]);
        assert_eq!(g.warm_packages, ["acme.util@1.0.0"]);
    }

    #[test]
    fn a_galaxy_yml_contributes_its_pinned_dependencies() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("galaxy.yml"),
            "namespace: acme\n\
             name: util\n\
             version: 1.0.0\n\
             dependencies:\n\
             \x20 community.general: 13.4.0\n\
             \x20 community.docker: \">=4.0.0\"\n",
        )
        .unwrap();
        let regs = suggest_registries(dir.path(), 0);
        let g = find(&regs, "galaxy");
        // `version: 1.0.0` at the top level is the collection's *own* version,
        // not a dependency, and must not be warmed.
        assert_eq!(g.warm_packages, ["community.general@13.4.0"]);
    }

    #[test]
    fn a_flow_style_collections_list_says_it_was_not_read() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("requirements.yml"),
            "collections: [community.general, community.docker]\n",
        )
        .unwrap();
        let regs = suggest_registries(dir.path(), 0);
        let g = find(&regs, "galaxy");
        assert!(g.warm_packages.is_empty());
        let note = g.note.as_deref().unwrap_or_default();
        assert!(note.contains("flow style"), "{note}");
    }

    #[test]
    fn nothing_pinned_warms_nothing_and_says_why() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("requirements.yml"),
            "collections:\n  - name: community.general\n    version: \"*\"\n",
        )
        .unwrap();
        let regs = suggest_registries(dir.path(), 0);
        let g = find(&regs, "galaxy");
        assert!(g.warm_packages.is_empty());
        assert!(g
            .note
            .as_deref()
            .unwrap_or_default()
            .contains("no collection is pinned"));
    }

    #[test]
    fn a_chart_lock_maps_each_repository_to_a_helm_registry_and_warms_its_pins() {
        let dir = TempDir::new().unwrap();
        // Both present: the lock wins, because it names what was resolved.
        std::fs::write(
            dir.path().join("Chart.yaml"),
            "apiVersion: v2\nname: app\nversion: 0.1.0\ndependencies:\n\
             \x20 - name: cert-manager\n    version: \"~1.21\"\n    repository: https://charts.jetstack.io\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Chart.lock"),
            "dependencies:\n\
             - name: cert-manager\n  repository: https://charts.jetstack.io/\n  version: v1.21.2\n\
             - name: ingress-nginx\n  repository: https://kubernetes.github.io/ingress-nginx\n  version: 4.15.1\n\
             - name: trust-manager\n  repository: https://charts.jetstack.io\n  version: 0.12.0\n\
             digest: sha256:00\ngenerated: \"2026-10-17T00:00:00Z\"\n",
        )
        .unwrap();
        let regs = suggest_registries(dir.path(), 0);
        let jet = find(&regs, "helm-charts-jetstack-io");
        assert_eq!(jet.registry_type, "helm");
        assert_eq!(jet.sources, ["Chart.lock"]);
        assert_eq!(
            jet.upstreams,
            ["https://charts.jetstack.io"],
            "one repository, slash trimmed"
        );
        assert_eq!(
            jet.warm_packages,
            ["cert-manager@v1.21.2", "trust-manager@0.12.0"]
        );
        assert_eq!(jet.note, None);
        assert_eq!(
            jet.proxy_url("https://hub.example.com/"),
            "https://hub.example.com/proxy/helm-charts-jetstack-io/helm",
            "the URL `helm repo add` takes"
        );
        let nginx = find(&regs, "helm-kubernetes-github-io-ingress-nginx");
        assert_eq!(
            nginx.upstreams,
            ["https://kubernetes.github.io/ingress-nginx"]
        );
        assert_eq!(nginx.warm_packages, ["ingress-nginx@4.15.1"]);
    }

    #[test]
    fn a_chart_yaml_warms_exact_pins_only_and_reports_what_it_cannot_proxy() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("Chart.yaml"),
            "apiVersion: v2\nname: app\nversion: 0.1.0\ndependencies:\n\
             \x20 - name: postgresql\n    version: 12.1.6\n    repository: https://charts.example.com\n\
             \x20 - name: redis\n    version: 17.x.x\n    repository: https://charts.example.com\n\
             \x20 - name: valkey\n    version: 2.0.0\n    repository: oci://registry-1.docker.io/bitnamicharts\n\
             \x20 - name: common\n    version: 1.0.0\n    repository: \"@bitnami\"\n\
             \x20 - name: local\n    version: 0.1.0\n    repository: file://../local\n",
        )
        .unwrap();
        let regs = suggest_registries(dir.path(), 0);
        assert_eq!(regs.iter().filter(|r| r.registry_type == "helm").count(), 1);
        let h = find(&regs, "helm-charts-example-com");
        assert_eq!(h.sources, ["Chart.yaml"]);
        assert_eq!(h.warm_packages, ["postgresql@12.1.6"], "17.x.x is a range");
        let note = h
            .note
            .as_deref()
            .expect("the skipped dependencies are named");
        for skipped in ["valkey (oci://", "common (@bitnami)", "local (file://"] {
            assert!(note.contains(skipped), "{skipped} missing from {note}");
        }
    }

    #[test]
    fn two_repositories_folding_to_one_name_get_two_registries() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("Chart.yaml"),
            "apiVersion: v2\nname: app\nversion: 0.1.0\ndependencies:\n\
             \x20 - name: a\n    version: 1.0.0\n    repository: https://charts.example.com\n\
             \x20 - name: b\n    version: 1.0.0\n    repository: http://charts.example.com\n",
        )
        .unwrap();
        let regs = suggest_registries(dir.path(), 0);
        let helm: Vec<_> = regs.iter().filter(|r| r.registry_type == "helm").collect();
        assert_eq!(helm.len(), 2, "{helm:?}");
        assert!(helm.iter().all(|r| r.upstreams.len() == 1), "{helm:?}");
        find(&regs, "helm-charts-example-com-2");
    }

    #[test]
    fn exact_chart_versions_are_told_from_ranges() {
        for v in ["1.2.3", "v1.21.2", "1.0.0-rc.1", "2.0.0+build.5"] {
            assert!(is_exact_chart_version(v), "{v}");
        }
        for v in [
            "1.2",
            "1.2.x",
            "~1.21",
            "^1.0.0",
            ">=1.0.0",
            ">=1.0.0 <2.0.0",
            "1.0.0-rc.1 || 2.0.0",
            "*",
            "",
        ] {
            assert!(!is_exact_chart_version(v), "{v}");
        }
    }

    #[test]
    fn a_project_with_no_chart_gets_no_helm_registry() {
        let dir = TempDir::new().unwrap();
        let regs = suggest_registries(dir.path(), 0);
        assert!(regs.iter().all(|r| r.registry_type != "helm"));
    }

    #[test]
    fn a_project_with_no_ansible_files_gets_no_galaxy_registry() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(".nvmrc"), "22.11.0\n").unwrap();
        let regs = suggest_registries(dir.path(), 0);
        assert!(!names(&regs).contains(&"galaxy"), "{:?}", names(&regs));
    }

    const SAMPLE_LOCK: &str = r#"
[[tools."aqua:EmbarkStudios/cargo-deny"]]
version = "0.20.2"
backend = "aqua:EmbarkStudios/cargo-deny"

[tools."aqua:EmbarkStudios/cargo-deny"."platforms.linux-x64"]
url = "https://github.com/EmbarkStudios/cargo-deny/releases/download/0.20.2/cargo-deny.tar.gz"

[[tools."cargo:cargo-audit"]]
version = "0.22.2"
backend = "cargo:cargo-audit"

[[tools.node]]
version = "24.18.0"
backend = "core:node"

[tools.node."platforms.linux-x64"]
url = "https://nodejs.org/dist/v24.18.0/node-v24.18.0-linux-x64.tar.gz"

[[tools.rust]]
version = "1.97.1"
backend = "core:rust"

[[tools.semgrep]]
version = "1.170.0"
backend = "pipx:semgrep"

[[tools.helm]]
version = "4.2.3"
backend = "aqua:helm/helm"

[tools.helm."platforms.linux-x64"]
url = "https://get.helm.sh/helm-v4.2.3-linux-amd64.tar.gz"
"#;

    #[test]
    fn lock_scan_covers_typed_generic_and_urlless_entries() {
        let mut acc = Accumulator::default();
        collect_from_mise_lock(SAMPLE_LOCK, &mut acc);
        let regs = acc.finish();
        let got = names(&regs);
        for expected in ["github", "cargo", "pypi", "node", "rust", "helm-bin"] {
            assert!(got.contains(&expected), "expected {expected} in {got:?}");
        }
        // helm's *binary* is a generic mirror even though the tool is installed
        // via the aqua backend — the URL wins over the backend prefix.
        assert_eq!(find(&regs, "helm-bin").registry_type, "generic");
    }

    #[test]
    fn one_github_entry_covers_every_aqua_tool() {
        let lock = r#"
[[tools.a]]
backend = "aqua:x/a"
[tools.a."platforms.linux-x64"]
url = "https://github.com/x/a/releases/download/1/a.tar.gz"

[[tools.b]]
backend = "aqua:y/b"
[tools.b."platforms.linux-x64"]
url = "https://github.com/y/b/releases/download/2/b.tar.gz"
"#;
        let mut acc = Accumulator::default();
        collect_from_mise_lock(lock, &mut acc);
        let regs = acc.finish();
        assert_eq!(names(&regs), vec!["github"]);
        // Both tools are recorded as reasons, so the user can see what needs it.
        assert_eq!(find(&regs, "github").sources.len(), 2);
    }

    #[test]
    fn multiple_platform_urls_on_one_host_merge_their_allowlists() {
        let lock = r#"
[[tools.thing]]
backend = "http"
[tools.thing."platforms.linux-x64"]
url = "https://vendor.example.com/rel/1.0/thing-linux.tar.gz"
[tools.thing."platforms.macos-arm64"]
url = "https://vendor.example.com/rel/1.0/thing-macos.tar.gz"
"#;
        let mut acc = Accumulator::default();
        collect_from_mise_lock(lock, &mut acc);
        let regs = acc.finish();
        let reg = find(&regs, "vendor-example");
        assert_eq!(
            reg.path_allow,
            vec!["rel/1.0/thing-linux.tar.gz", "rel/1.0/thing-macos.tar.gz"]
        );
    }

    #[test]
    fn malformed_lock_is_ignored_rather_than_fatal() {
        let mut acc = Accumulator::default();
        collect_from_mise_lock("this is not : valid toml [[[", &mut acc);
        assert!(acc.finish().is_empty());
    }

    #[test]
    fn mise_toml_without_lock_falls_back_to_tool_names() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("mise.toml"),
            "[tools]\nnode = \"24\"\nrust = \"1.97\"\n\"cargo:cargo-audit\" = \"latest\"\ngitleaks = \"8\"\n",
        )
        .unwrap();
        let regs = suggest_registries(dir.path(), 0);
        let got = names(&regs);
        assert!(got.contains(&"node"), "{got:?}");
        assert!(got.contains(&"rust"), "{got:?}");
        assert!(got.contains(&"cargo"), "{got:?}");
        // `gitleaks` is unqualified → assumed to be aqua/ubi → GitHub, with a
        // caveat telling the user to run `mise lock`.
        let gh = find(&regs, "github");
        assert!(gh.note.as_deref().unwrap().contains("mise lock"));
    }

    #[test]
    fn lock_supersedes_mise_toml_when_both_exist() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("mise.toml"),
            "[tools]\nsomething-odd = \"1\"\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("mise.lock"),
            "[[tools.node]]\nbackend = \"core:node\"\n",
        )
        .unwrap();
        let regs = suggest_registries(dir.path(), 0);
        // The fuzzy mise.toml guess ("something-odd" → github) must not appear.
        assert_eq!(names(&regs), vec!["node"]);
    }

    #[test]
    fn project_manifests_contribute_typed_registries() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(dir.path().join("package.json"), r#"{"name":"x"}"#).unwrap();
        let regs = suggest_registries(dir.path(), 0);
        let got = names(&regs);
        assert!(got.contains(&"cargo"), "{got:?}");
        assert!(got.contains(&"npm"), "{got:?}");
    }

    #[test]
    fn go_modules_manifest_maps_to_the_goproxy_type_not_the_scanner_label() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("go.mod"), "module example.com/x\n").unwrap();
        let regs = suggest_registries(dir.path(), 0);
        assert_eq!(find(&regs, "go").registry_type, "goproxy");
    }

    #[test]
    fn empty_project_suggests_nothing() {
        let dir = TempDir::new().unwrap();
        assert!(suggest_registries(dir.path(), 0).is_empty());
    }

    // ── .nvmrc and .sdkmanrc (RFC 0010 §6.9) ─────────────────────────────────

    #[test]
    fn nvmrc_maps_to_a_nodedist_registry_and_warms_the_pinned_release() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(".nvmrc"), "22.11.0\n").unwrap();
        let regs = suggest_registries(dir.path(), 0);
        let node = find(&regs, "node");
        assert_eq!(node.registry_type, "nodedist");
        assert_eq!(node.sources, [".nvmrc"]);
        assert_eq!(
            node.warm_packages,
            ["node@v22.11.0"],
            "spelled as the tree spells it"
        );
        assert_eq!(
            node.resolved_env("https://hub.example.com"),
            NODEDIST_VARS
                .map(|k| (
                    k.to_owned(),
                    "https://hub.example.com/proxy/node/nodedist".to_owned()
                ))
                .to_vec(),
            "the same variables `setup env` exports"
        );

        let toml = render_toml(&regs, &[]);
        assert!(toml.contains("type       = \"nodedist\""), "{toml}");
        assert!(
            toml.contains("warm_packages = [\"node@v22.11.0\"]"),
            "{toml}"
        );
        assert!(
            toml.contains("\"releases:list\""),
            "listings take their own verb: {toml}"
        );
        toml::from_str::<toml::Value>(&toml).expect("the rendered block is valid TOML");
    }

    #[test]
    fn nvmrc_alias_warms_nothing_and_says_why() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(".nvmrc"), "lts/*\n").unwrap();
        let regs = suggest_registries(dir.path(), 0);
        let node = find(&regs, "node");
        assert!(node.warm_packages.is_empty());
        assert!(node.note.as_deref().unwrap_or("").contains("lts/*"));
        let with_v = registry_for_nvmrc("v20.18.0");
        assert_eq!(with_v.warm_packages, ["node@v20.18.0"]);
    }

    #[test]
    fn sdkmanrc_maps_to_an_sdkman_registry_with_both_client_variables() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join(".sdkmanrc"),
            "# Enable auto-env through the sdkman_auto_env config\njava=21.0.5-tem\nmaven = 3.9.9\n",
        )
        .unwrap();
        let regs = suggest_registries(dir.path(), 0);
        let sdk = find(&regs, "sdkman");
        assert_eq!(sdk.registry_type, "sdkman");
        assert_eq!(sdk.warm_packages, ["java@21.0.5-tem", "maven@3.9.9"]);
        assert_eq!(
            sdk.resolved_env("https://hub.example.com/"),
            [
                (
                    "SDKMAN_CANDIDATES_API".to_owned(),
                    "https://hub.example.com/proxy/sdkman/sdkman".to_owned()
                ),
                (
                    "SDKMAN_BROKER_API".to_owned(),
                    "https://hub.example.com/proxy/sdkman/sdkman/broker".to_owned()
                ),
            ]
        );
        let env = render_client_env(&regs, "https://hub.example.com");
        assert!(env.contains("export SDKMAN_BROKER_API="), "{env}");
        let toml = render_toml(&regs, &[]);
        toml::from_str::<toml::Value>(&toml).expect("the rendered block is valid TOML");
        assert!(toml.contains("[registries.cache]"), "{toml}");
    }

    #[test]
    fn rendered_toml_is_parseable_and_passes_the_generic_allowlist_rule() {
        let mut acc = Accumulator::default();
        collect_from_mise_lock(SAMPLE_LOCK, &mut acc);
        let regs = acc.finish();
        let rendered = render_toml(&regs, &[]);

        let parsed: toml::Value = toml::from_str(&rendered)
            .unwrap_or_else(|e| panic!("rendered TOML must parse: {e}\n{rendered}"));
        let entries = parsed["registries"].as_array().unwrap();
        assert_eq!(entries.len(), regs.len());
        // The server rejects a `generic` registry without upstreams + path_allow,
        // so the generated blocks must never need hand-editing to start up.
        for entry in entries {
            if entry["type"].as_str() == Some("generic") {
                assert!(entry.get("upstreams").is_some(), "generic needs upstreams");
                let allow = entry["path_allow"].as_array().unwrap();
                assert!(!allow.is_empty(), "generic needs a non-empty path_allow");
            }
        }
    }

    #[test]
    fn client_env_resolves_the_proxy_placeholder() {
        let mut acc = Accumulator::default();
        collect_from_mise_lock(SAMPLE_LOCK, &mut acc);
        let regs = acc.finish();
        let env = render_client_env(&regs, "https://hub.example.com/");
        assert!(
            env.contains(
                "export NODEJS_ORG_MIRROR=\"https://hub.example.com/proxy/node/nodedist\""
            ),
            "{env}"
        );
        assert!(
            env.contains(
                "export RUSTUP_UPDATE_ROOT=\"https://hub.example.com/proxy/rust/rustup/rustup\""
            ),
            "{env}"
        );
        assert!(
            !env.contains("{proxy}"),
            "placeholder must be resolved: {env}"
        );
    }

    /// The catch-all matches every https host, this server's included. An
    /// operator who already pointed a backend at BatleHub would have that URL
    /// rewritten into the `501` sink — a working registry turned into an
    /// "unmirrored host" by the rule meant to find unmirrored hosts. Matching
    /// stops at the first hit, so the identity rule has to come first.
    #[test]
    fn the_proxys_own_urls_are_left_alone_ahead_of_the_catch_all() {
        let regs = vec![registry_for_url(
            "https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz",
            "s",
        )
        .unwrap()];
        let out = render_mise_toml(&regs, "https://hub.example.com", false, true);
        let identity = out
            .find("hub\\\\.example\\\\.com")
            .unwrap_or_else(|| panic!("the identity rule is emitted:\n{out}"));
        let catch_all = out
            .find("^https://([^/]+)/(.*)")
            .unwrap_or_else(|| panic!("the catch-all is emitted:\n{out}"));
        assert!(
            identity < catch_all,
            "the identity rule must precede the catch-all, or the catch-all swallows it:\n{out}"
        );
        assert!(out.contains(r#"= "https://hub.example.com/$1""#), "{out}");
    }

    #[test]
    fn an_identity_rule_needs_a_host_to_anchor_on() {
        assert!(mise_proxy_identity_rule("https://hub.example.com/").is_some());
        assert_eq!(
            mise_proxy_identity_rule("http://localhost:8080"),
            Some((
                r"^http://localhost:8080/(.*)".to_owned(),
                "http://localhost:8080/$1".to_owned()
            ))
        );
        assert!(mise_proxy_identity_rule("not-a-url").is_none());
    }

    /// The plan's paths and the rewrite rules must agree, because the plan's
    /// path is what `mise seed` fetches. A `generic` mirror routes as
    /// `/proxy/{name}/generic/{path}`; a plan that dropped the segment seeded
    /// nothing but 404s, and the operator learned it on the disconnected side.
    #[test]
    fn a_plans_path_carries_the_same_type_segment_as_the_rewrite_rule() {
        assert_eq!(
            proxy_path_for(
                "https://nodejs.org/dist/v24.18.0/node-v24.18.0-linux-x64.tar.gz",
                "node-dist",
                "generic"
            )
            .as_deref(),
            Some("/proxy/node-dist/generic/v24.18.0/node-v24.18.0-linux-x64.tar.gz")
        );
        let suggested =
            registry_for_url("https://ziglang.org/download/0.15.2/x.tar.xz", "s").unwrap();
        assert!(suggested
            .proxy_url("https://hub.example.com")
            .ends_with("/proxy/zig-dl/generic"));
        // GitHub routes straight off the name, and gains no segment either way.
        assert_eq!(
            proxy_path_for(
                "https://api.github.com/repos/cli/cli/releases/tags/v2.60.0",
                "gh",
                "github"
            )
            .as_deref(),
            Some("/proxy/gh/cli/cli/releases/tags/v2.60.0")
        );
    }

    #[test]
    fn proxy_url_adds_the_type_segment_only_for_path_addressed_types() {
        let generic =
            registry_for_url("https://ziglang.org/download/0.15.2/x.tar.xz", "s").unwrap();
        assert_eq!(
            generic.proxy_url("https://hub.example.com"),
            "https://hub.example.com/proxy/zig-dl/generic"
        );
        // GitHub routes as /proxy/{name}/{owner}/{repo}/… — a `/github/` segment
        // here would 404 every rewritten URL.
        let github = registry_for_backend("aqua:x/y", "s").pop().unwrap();
        assert_eq!(
            github.proxy_url("https://hub.example.com"),
            "https://hub.example.com/proxy/github"
        );
    }

    #[test]
    fn regex_escape_escapes_dots_so_a_host_cannot_match_loosely() {
        assert_eq!(
            regex_escape("https://nodejs.org/dist"),
            r"https://nodejs\.org/dist"
        );
        assert_eq!(regex_escape("a+b?c"), r"a\+b\?c");
    }

    #[test]
    fn toml_quote_doubles_backslashes() {
        // A lone `\.` is a TOML parse error; mise then drops the whole settings
        // block while still exiting 0, so this doubling is load-bearing.
        assert_eq!(
            toml_quote(r"regex:^https://a\.b/(.+)"),
            r#""regex:^https://a\\.b/(.+)""#
        );
    }

    #[test]
    fn generic_mise_rule_is_derived_from_the_upstream() {
        let reg = registry_for_url(
            "https://builds.hex.pm/builds/elixir/v1.18.4-otp-27.zip",
            "mise.lock: elixir",
        )
        .unwrap();
        let rules = reg.mise_url_replacements("https://hub.example.com");
        assert_eq!(
            rules,
            vec![(
                r"^https://builds\.hex\.pm/builds/(.+)".to_owned(),
                "https://hub.example.com/proxy/hex-builds/generic/$1".to_owned()
            )]
        );
    }

    #[test]
    fn github_mise_rules_cover_the_api_asset_path_aqua_actually_uses() {
        let reg = registry_for_backend("aqua:x/y", "mise.lock: y")
            .pop()
            .unwrap();
        let rules = reg.mise_url_replacements("https://hub.example.com");
        // Verified against `mise install aqua:…`: assets are fetched from
        // api.github.com/repos/{o}/{r}/releases/assets/{id}, so this rule is the
        // one that must be present.
        assert!(
            rules
                .iter()
                .any(|(p, _)| p.starts_with(r"^https://api\.github\.com/repos/")),
            "{rules:?}"
        );
        assert_eq!(rules.len(), 6);
    }

    /// mise's `forgejo:` backend downloads from `{forge}/attachments/{uuid}`,
    /// which it builds itself — the release document's own URLs route only the
    /// checksum sibling. The closed-world suite found this with the binary
    /// going to codeberg while the `.sha256` beside it came through the proxy.
    #[test]
    fn forgejo_mise_rules_cover_the_attachment_path_mise_actually_downloads_from() {
        let reg = registry_for_url(
            "https://codeberg.org/api/v1/repos/forgejo/forgejo/releases/tags/v16.0.4",
            "mise.lock: forgejo",
        )
        .unwrap();
        assert_eq!(reg.registry_type, "forgejo");

        let rules = reg.mise_url_replacements("https://hub.example.com");
        let attachments = rules
            .iter()
            .find(|(p, _)| p.contains("/attachments/"))
            .unwrap_or_else(|| panic!("no attachment rule in {rules:?}"));
        assert_eq!(
            attachments.1,
            "https://hub.example.com/proxy/forgejo/attachments/$1"
        );
        assert_eq!(rules.len(), 3);
    }

    /// RFC 0047 phase 1: each backend gets the verdict its download path
    /// earns — a registry, a note, or a reason naming what it reads.
    #[test]
    fn backend_verdicts_of_rfc_0047_phase_1() {
        let ty = |b: &str| registry_for_backend(b, "s").pop().map(|r| r.registry_type);
        assert_eq!(ty("dotnet:dotnet-outdated-tool").as_deref(), Some("nuget"));
        assert_eq!(
            ty("packslip:github.com/acme/tool").as_deref(),
            Some("github")
        );
        assert_eq!(ty("packslip:acme/tool").as_deref(), Some("github"));
        assert_eq!(ty("spm:gitlab.com/acme/pkg").as_deref(), Some("gitlab"));
        assert_eq!(ty("spm:acme/pkg").as_deref(), Some("github"));
        for supported in [
            "dotnet:x",
            "packslip:acme/tool",
            "spm:acme/pkg",
            "vfox:jdx/vfox-ant",
        ] {
            assert_eq!(unsupported_backend_reason(supported), None, "{supported}");
        }

        // A packslip host is read for its `.well-known` document, which
        // nothing here mirrors.
        assert_eq!(ty("packslip:tool.example.com"), None);
        let reason = unsupported_backend_reason("packslip:tool.example.com").unwrap();
        assert!(
            reason.contains("https://tool.example.com/.well-known/packslip.json"),
            "{reason}"
        );

        // asdf keeps its reason, now on one line.
        let asdf = unsupported_backend_reason("asdf:mise-plugins/mise-postgres").unwrap();
        assert!(
            asdf.contains("git-fetched") && !asdf.contains("  "),
            "{asdf}"
        );

        // The caveats ride the suggestion.
        let aqua = registry_for_backend("aqua:kubernetes/kubectl", "s")
            .pop()
            .unwrap();
        assert!(aqua.note.unwrap().contains("type `http`"));
        assert!(registry_for_backend("spm:acme/pkg", "s")
            .pop()
            .unwrap()
            .note
            .unwrap()
            .contains("git"));
        assert!(registry_for_backend("cargo:ripgrep", "s")
            .pop()
            .unwrap()
            .note
            .is_none());

        // vfox: a lock-less plugin is a note, and the known shell-outs carry
        // their own, whatever the plugin's spelling.
        assert!(lockless_backend_note("vfox:jdx/vfox-ant")
            .unwrap()
            .contains("mise lock"));
        assert!(lockless_backend_note("aqua:x/y").is_none());
        let php = vfox_subprocess_note("vfox:mise-plugins/vfox-php").unwrap();
        assert!(php.contains("configure, make"), "{php}");
        assert!(vfox_subprocess_note("vfox:poetry").is_some());
        assert!(vfox_subprocess_note("vfox:jdx/vfox-ant").is_none());
    }

    /// mise's `conda:` backend runs rattler in-process, so unlike `pipx:` it
    /// needs a rewrite rule, and the plan and the rule agree on the path.
    #[test]
    fn conda_backend_is_supported_and_routes_the_conda_forge_channel() {
        assert_eq!(unsupported_backend_reason("conda:ffmpeg"), None);
        let reg = registry_for_backend("conda:ffmpeg", "mise.lock: ffmpeg")
            .pop()
            .unwrap();
        let rules = reg.mise_url_replacements("https://hub.example.com");
        assert_eq!(
            rules,
            [(
                r"^https://conda\.anaconda\.org/conda-forge/(.+)".to_owned(),
                "https://hub.example.com/proxy/conda/$1".to_owned()
            )]
        );
        assert_eq!(
            proxy_path_for(
                "https://conda.anaconda.org/conda-forge/linux-64/ffmpeg-7.1.1-h0_0.conda",
                "conda",
                "conda"
            )
            .as_deref(),
            Some("/proxy/conda/linux-64/ffmpeg-7.1.1-h0_0.conda")
        );
        // Another channel on the same host is not conda-forge's registry.
        assert_eq!(
            proxy_path_for(
                "https://conda.anaconda.org/bioconda/noarch/x-1-0.tar.bz2",
                "conda",
                "conda"
            ),
            None
        );
    }

    #[test]
    fn subprocess_driven_types_get_no_mise_rule() {
        let reg = registry_for_backend("pipx:semgrep", "mise.lock: semgrep")
            .pop()
            .unwrap();
        assert!(reg
            .mise_url_replacements("https://hub.example.com")
            .is_empty());
    }

    #[test]
    fn rendered_mise_block_is_valid_toml_and_lands_under_settings() {
        let mut acc = Accumulator::default();
        collect_from_mise_lock(SAMPLE_LOCK, &mut acc);
        let regs = acc.finish();
        let rendered = render_mise_toml(&regs, "https://hub.example.com", false, false);

        let parsed: toml::Value = toml::from_str(&rendered)
            .unwrap_or_else(|e| panic!("mise block must parse as TOML: {e}\n{rendered}"));
        let repl = parsed["settings"]["url_replacements"].as_table().unwrap();

        // The TOML round-trip proves the escaping: the key holds a single
        // backslash once unescaped.
        let node_key = r"regex:^https://nodejs\.org/dist/(.+)";
        assert_eq!(
            repl[node_key].as_str(),
            Some("https://hub.example.com/proxy/node/nodedist/$1"),
            "{rendered}"
        );
        // mise's own node setting, and rustup's variables where a subprocess
        // reads them; rustup is not "not rewritable".
        assert_eq!(
            parsed["settings"]["node"]["mirror_url"].as_str(),
            Some("https://hub.example.com/proxy/node/nodedist/")
        );
        assert_eq!(
            parsed["env"]["RUSTUP_DIST_SERVER"].as_str(),
            Some("https://hub.example.com/proxy/rust/rustup")
        );
        assert!(
            !rendered.contains("rustup,") && !rendered.contains("pypi, rustup"),
            "{rendered}"
        );
        // No zig in the lock, no zig setting.
        assert!(parsed["settings"].get("zig").is_none());
        assert!(repl.contains_key(r"regex:^https://api\.github\.com/repos/(.+)"));
        // pypi came from `pipx:semgrep` and cannot be rewritten — it must be
        // called out rather than silently missing.
        assert!(rendered.contains("Not rewritable here: pypi"), "{rendered}");
    }

    /// RFC 0048 §4.5: the refusal is printed beside the registries, the zig
    /// setting travels with the zig rule, and the swift mirror carries its
    /// own size limit — and the whole output loads on the server.
    #[test]
    fn core_toolchains_render_their_presets_settings_and_refusals() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("mise.lock"),
            r#"
[[tools.zig]]
version = "0.15.2"
backend = "core:zig"
[tools.zig."platforms.linux-x64"]
url = "https://ziglang.org/download/0.15.2/zig-x86_64-linux-0.15.2.tar.xz"

[[tools.elixir]]
version = "1.18.4-otp-27"
backend = "core:elixir"
[tools.elixir."platforms.linux-x64"]
url = "https://builds.hex.pm/builds/elixir/v1.18.4-otp-27.zip"

[[tools.swift]]
version = "6.2"
backend = "core:swift"

[[tools.dotnet]]
version = "9.0.100"
backend = "core:dotnet"
"#,
        )
        .unwrap();
        let (regs, unroutable) = suggest(dir.path(), 0);
        assert_eq!(
            names(&regs),
            ["hex-builds", "swift-api", "swift-dl", "zig-dl"]
        );
        assert_eq!(unroutable.len(), 1);
        assert!(unroutable[0].starts_with("dotnet (core:dotnet) — mise runs dotnet-install.sh"));

        let rendered = render_toml(&regs, &unroutable);
        assert!(
            rendered.contains("\n# Not routable through BatleHub:\n#   dotnet (core:dotnet)"),
            "{rendered}"
        );
        assert!(
            rendered.contains("max_artifact_size_bytes = 1700000000"),
            "{rendered}"
        );
        assert!(rendered.contains("  \"otp/*/*/builds.txt\","), "{rendered}");
        // What `registry suggest` prints is a config the server loads.
        let config = format!(
            "[server]\nhost = \"127.0.0.1\"\nport = 8080\n[database]\ntype = \"postgresql\"\n\
             url = \"postgresql://u:p@localhost/db\"\n[storage]\ntype = \"filesystem\"\n\
             path = \"./tmp\"\n{rendered}"
        );
        batlehub_config::load_from_str(&config).unwrap_or_else(|e| panic!("{e:#}\n{config}"));

        let mise = render_mise_toml(&regs, "https://hub.example.com", false, false);
        let parsed: toml::Value = toml::from_str(&mise).unwrap();
        assert_eq!(
            parsed["settings"]["zig"]["use_community_mirrors"].as_bool(),
            Some(false),
            "{mise}"
        );
        let repl = parsed["settings"]["url_replacements"].as_table().unwrap();
        assert_eq!(
            repl[r"regex:^https://www\.swift\.org/api/v1/install/(.+)"].as_str(),
            Some("https://hub.example.com/proxy/swift-api/generic/$1")
        );

        // The same refusal from a mise.toml, and no GitHub guess for it.
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("mise.toml"), "[tools]\ndotnet = \"9\"\n").unwrap();
        let (regs, unroutable) = suggest(dir.path(), 0);
        assert!(regs.is_empty(), "{:?}", names(&regs));
        assert_eq!(unroutable.len(), 1);
    }

    #[test]
    fn commented_mise_block_has_no_active_toml() {
        let mut acc = Accumulator::default();
        collect_from_mise_lock(SAMPLE_LOCK, &mut acc);
        let rendered = render_mise_toml(&acc.finish(), "https://hub.example.com", true, false);
        let parsed: toml::Value = toml::from_str(&rendered).expect("commented block must parse");
        assert!(
            parsed.as_table().is_none_or(|t| t.is_empty()),
            "a commented block must define nothing: {parsed:?}"
        );
        // Body lines gain one comment level; the generator's own header comments
        // gain a second so they survive the strip.
        assert!(
            rendered
                .lines()
                .any(|l| l == "# [settings.url_replacements]"),
            "{rendered}"
        );
        assert!(
            rendered.lines().any(|l| l.starts_with("# # Generated by")),
            "{rendered}"
        );
    }

    #[test]
    fn stripping_one_comment_level_yields_a_working_block() {
        // This is the operation the instructions ask the user to perform, so it
        // has to produce valid TOML — including for the generator's own header
        // lines, which must survive as comments rather than become bare prose.
        let mut acc = Accumulator::default();
        collect_from_mise_lock(SAMPLE_LOCK, &mut acc);
        let regs = acc.finish();
        let commented = render_mise_toml(&regs, "https://hub.example.com", true, false);

        let uncommented: String = commented
            .lines()
            .map(|l| l.strip_prefix("# ").unwrap_or(l))
            .collect::<Vec<_>>()
            .join("\n");

        let parsed: toml::Value = toml::from_str(&uncommented)
            .unwrap_or_else(|e| panic!("uncommented block must parse: {e}\n{uncommented}"));
        assert_eq!(
            parsed["settings"]["url_replacements"][r"regex:^https://nodejs\.org/dist/(.+)"]
                .as_str(),
            Some("https://hub.example.com/proxy/node/nodedist/$1")
        );
        // …and it must be byte-identical to the uncommented rendering.
        let direct = render_mise_toml(&regs, "https://hub.example.com", false, false);
        let strip_header = |s: &str| {
            s.lines()
                .filter(|l| !l.starts_with("# Uncomment"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(strip_header(&uncommented), strip_header(&direct));
    }

    #[test]
    fn client_env_falls_back_to_proxy_urls_when_no_variable_exists() {
        let regs = vec![SuggestedRegistry {
            name: "helm-bin".to_owned(),
            registry_type: "generic".to_owned(),
            upstreams: vec!["https://get.helm.sh".to_owned()],
            path_allow: vec!["helm-v*".to_owned()],
            sources: vec!["mise.lock: helm".to_owned()],
            client_env: Vec::new(),
            warm_packages: Vec::new(),
            revalidate: Vec::new(),
            max_artifact_size_bytes: None,
            note: None,
        }];
        let env = render_client_env(&regs, "https://hub.example.com");
        assert!(
            env.contains("https://hub.example.com/proxy/helm-bin/generic/<path>"),
            "{env}"
        );
    }
}
