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
    client_env: &'static [(&'static str, &'static str)],
}

/// Hosts that speak a package protocol BatleHub has a typed adapter for. The
/// suggested registry uses that adapter's default upstream, so no `upstreams`
/// entry is emitted.
const TYPED_HOSTS: &[(&str, &str, &str)] = &[
    // (host, registry type, suggested name)
    ("api.github.com", "github", "github"),
    ("github.com", "github", "github"),
    ("codeload.github.com", "github", "github"),
    ("raw.githubusercontent.com", "github", "github"),
    ("objects.githubusercontent.com", "github", "github"),
    ("registry.npmjs.org", "npm", "npm"),
    ("crates.io", "cargo", "cargo"),
    ("static.crates.io", "cargo", "cargo"),
    ("index.crates.io", "cargo", "cargo"),
    ("pypi.org", "pypi", "pypi"),
    ("files.pythonhosted.org", "pypi", "pypi"),
    ("proxy.golang.org", "goproxy", "go"),
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

/// Hosts with no package protocol at all, for which we know a sensible upstream
/// root, allowlist and client mirror variable.
fn generic_preset(host: &str) -> Option<GenericPreset> {
    let preset = match host {
        "nodejs.org" => GenericPreset {
            name: "node-dist",
            upstream: "https://nodejs.org/dist",
            path_allow: &["v*/**"],
            client_env: &[("NODEJS_ORG_MIRROR", "{proxy}")],
        },
        "static.rust-lang.org" => GenericPreset {
            name: "rust-dist",
            upstream: "https://static.rust-lang.org",
            path_allow: &["dist/**", "rustup/**"],
            client_env: &[
                ("RUSTUP_DIST_SERVER", "{proxy}"),
                ("RUSTUP_UPDATE_ROOT", "{proxy}/rustup"),
            ],
        },
        "dl.google.com" => GenericPreset {
            // The Go *toolchain* tarballs. Go *modules* are a separate
            // `type = "goproxy"` registry — see proxy.golang.org above.
            name: "go-dl",
            upstream: "https://dl.google.com/go",
            path_allow: &["go*"],
            client_env: &[],
        },
        "get.helm.sh" => GenericPreset {
            name: "helm-bin",
            upstream: "https://get.helm.sh",
            path_allow: &["helm-v*"],
            client_env: &[],
        },
        "dl.min.io" => GenericPreset {
            name: "minio-dl",
            upstream: "https://dl.min.io",
            path_allow: &["client/**", "server/**"],
            client_env: &[],
        },
        "binaries.sonarsource.com" => GenericPreset {
            name: "sonar-binaries",
            upstream: "https://binaries.sonarsource.com",
            path_allow: &["Distribution/**"],
            client_env: &[],
        },
        _ => return None,
    };
    Some(preset)
}

/// Object-storage hosts where the first path segment is a tenant bucket. The
/// bucket must be part of the upstream root, otherwise the mirror would relay
/// every other public bucket on the same host.
const BUCKET_HOSTS: &[&str] = &[
    "storage.googleapis.com",
    "s3.amazonaws.com",
    "objects.githubusercontent.com",
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
        let (host, _) = split_url(url)?;
        let preset = generic_preset(host)?;
        let upstream = preset.upstream.trim_end_matches('/');
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
    let base = format!("/proxy/{registry_name}");
    match registry_type.parse::<RegistryKind>() {
        Ok(kind)
            if kind.is_path_addressed()
                || matches!(kind, RegistryKind::Nodedist | RegistryKind::Sdkman) =>
        {
            format!("{base}/{kind}")
        }
        _ => base,
    }
}

/// The registry *kind* a host's downloads go through, for RFC 0008's plan.
///
/// Same table the suggester reads, asked a narrower question: planning needs
/// to know whether *some* configured registry of that kind mirrors the host,
/// not what a new registry should be called.
pub(crate) fn registry_kind_for_host(host: &str) -> Option<&'static str> {
    if let Some((_, kind, _)) = TYPED_HOSTS.iter().find(|(h, _, _)| *h == host) {
        return Some(kind);
    }
    // A generic mirror can front any host, so a host with a preset is
    // mirrorable as `generic` — and one without is not mirrorable at all
    // until an operator writes a registry for it.
    generic_preset(host).map(|_| "generic")
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
    {
        return None;
    }
    if backend.starts_with("asdf:") || backend.starts_with("vfox:") {
        return Some(format!(
            "{backend}: a git-fetched plugin backend; mise clones it, and there is no HTTP path              through BatleHub"
        ));
    }
    if backend.starts_with("core:") {
        // `core:*` downloads from a fixed host, which the URL rules cover
        // when a registry fronts it — and `unmirrored_hosts` says so when
        // none does.
        return None;
    }
    Some(format!(
        "{backend}: no registry kind fronts this backend, and it downloads nothing over HTTP that          a rewrite rule could catch"
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
    // aqua and ubi both resolve to GitHub release assets.
    ("aqua:", "github", "github"),
    ("ubi:", "github", "github"),
    ("github:", "github", "github"),
];

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
pub fn suggest_registries(root: &Path, depth: usize) -> Vec<SuggestedRegistry> {
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

    acc.finish()
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
        client_env: vec![("NVM_NODEJS_ORG_MIRROR".to_owned(), "{proxy}".to_owned())],
        warm_packages,
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
        note: None,
    }
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
    // Every `platforms.*` sub-table carries a `url` for one platform.
    for sub in table.values() {
        let Some(url) = sub.get("url").and_then(|u| u.as_str()) else {
            continue;
        };
        saw_url = true;
        if let Some(reg) = registry_for_url(url, source) {
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
    if let Some(reg) = registry_for_backend(backend, source) {
        acc.add(reg);
    }
}

/// Map one concrete download URL onto a registry.
fn registry_for_url(url: &str, source: &str) -> Option<SuggestedRegistry> {
    let (host, path) = split_url(url)?;

    if let Some((_, registry_type, name)) = TYPED_HOSTS.iter().find(|(h, _, _)| *h == host) {
        return Some(SuggestedRegistry {
            name: (*name).to_owned(),
            registry_type: (*registry_type).to_owned(),
            upstreams: Vec::new(),
            path_allow: Vec::new(),
            sources: vec![source.to_owned()],
            client_env: Vec::new(),
            warm_packages: Vec::new(),
            note: None,
        });
    }

    if let Some(preset) = generic_preset(host) {
        // The preset upstream may include a path prefix (e.g. `/dist`, `/go`);
        // the allowlist is relative to it, so the observed path is not reused.
        return Some(SuggestedRegistry {
            name: preset.name.to_owned(),
            registry_type: "generic".to_owned(),
            upstreams: vec![preset.upstream.to_owned()],
            path_allow: preset.path_allow.iter().map(|s| (*s).to_owned()).collect(),
            sources: vec![source.to_owned()],
            client_env: preset
                .client_env
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            warm_packages: Vec::new(),
            note: None,
        });
    }

    Some(generic_from_unknown_url(host, path, source))
}

/// A `generic` mirror for a host we have no preset for. The upstream is the host
/// (plus the bucket segment on object-storage hosts) and the allowlist is the
/// single exact path observed — narrow by construction, since we have no basis
/// for guessing which parts of it are version components.
fn generic_from_unknown_url(host: &str, path: &str, source: &str) -> SuggestedRegistry {
    let is_bucket_host = BUCKET_HOSTS.contains(&host);
    let (upstream, allow_path, name) = if is_bucket_host {
        match path.split_once('/') {
            Some((bucket, rest)) if !bucket.is_empty() => (
                format!("https://{host}/{bucket}"),
                rest.to_owned(),
                slug_from_bucket(bucket),
            ),
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
            vec![allow_path]
        },
        sources: vec![source.to_owned()],
        client_env: Vec::new(),
        warm_packages: Vec::new(),
        note: Some(
            "no preset for this host — path_allow lists the exact locked paths; \
             widen it (or re-run this command) when the pinned version changes"
                .to_owned(),
        ),
    }
}

/// Map a mise backend string (`"aqua:cli/tool"`, `"cargo:ripgrep"`, `"core:rust"`)
/// onto a registry, for entries whose lock records no URL.
fn registry_for_backend(backend: &str, source: &str) -> Option<SuggestedRegistry> {
    if let Some((_, registry_type, name)) = BACKEND_REGISTRIES
        .iter()
        .find(|(prefix, _, _)| backend.starts_with(prefix))
    {
        return Some(SuggestedRegistry {
            name: (*name).to_owned(),
            registry_type: (*registry_type).to_owned(),
            upstreams: Vec::new(),
            path_allow: Vec::new(),
            sources: vec![source.to_owned()],
            client_env: Vec::new(),
            warm_packages: Vec::new(),
            note: None,
        });
    }

    // `core:*` toolchains download from a fixed host, so they resolve to the
    // same presets the URL path would have produced.
    let host = match backend {
        "core:rust" => "static.rust-lang.org",
        "core:node" => "nodejs.org",
        "core:go" => "dl.google.com",
        _ => return None,
    };
    let preset = generic_preset(host)?;
    Some(SuggestedRegistry {
        name: preset.name.to_owned(),
        registry_type: "generic".to_owned(),
        upstreams: vec![preset.upstream.to_owned()],
        path_allow: preset.path_allow.iter().map(|s| (*s).to_owned()).collect(),
        sources: vec![source.to_owned()],
        client_env: preset
            .client_env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        warm_packages: Vec::new(),
        note: None,
    })
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
        if let Some(reg) = registry_for_backend(tool_key, &source) {
            acc.add(reg);
            continue;
        }
        // A bare name: try it as a `core:` toolchain first.
        if let Some(reg) = registry_for_backend(&format!("core:{tool_key}"), &source) {
            acc.add(reg);
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

/// Curated rewrite rules for typed registries, as `(regex, replacement)` pairs
/// with single-backslash regex escapes and `{proxy}` for the registry's proxy
/// base URL.
///
/// GitHub needs six rules because mise reaches GitHub through several distinct
/// URL shapes. The `api.github.com/repos/…` rule is the load-bearing one: aqua
/// and ubi resolve *and* download assets through the API
/// (`/releases/assets/{id}`), not through `github.com/…/releases/download/…`.
fn typed_url_replacements(registry_type: &str) -> &'static [(&'static str, &'static str)] {
    match registry_type {
        "github" => &[
            // API: release listings, tag metadata, and asset downloads.
            (r"^https://api\.github\.com/repos/(.+)", "{proxy}/$1"),
            // Release asset binaries (browser_download_url).
            (
                r"^https://github\.com/([^/]+)/([^/]+)/releases/download/([^/]+)/(.+)",
                "{proxy}/$1/$2/releases/download/$3/$4",
            ),
            // Source tarballs.
            (
                r"^https://github\.com/([^/]+)/([^/]+)/archive/(?:refs/tags/)?(.+?)\.tar\.gz",
                "{proxy}/$1/$2/tarball/$3",
            ),
            (
                r"^https://codeload\.github\.com/([^/]+)/([^/]+)/tar\.gz/(?:refs/tags/)?(.+)",
                "{proxy}/$1/$2/tarball/$3",
            ),
            // Zip archives.
            (
                r"^https://github\.com/([^/]+)/([^/]+)/archive/(?:refs/tags/)?(.+?)\.zip",
                "{proxy}/$1/$2/zipball/$3",
            ),
            // Raw files (install scripts, manifests).
            (
                r"^https://raw\.githubusercontent\.com/([^/]+)/([^/]+)/([^/]+)/(.+)",
                "{proxy}/$1/$2/raw/$3/$4",
            ),
        ],
        "npm" => &[(r"^https://registry\.npmjs\.org/(.+)", "{proxy}/$1")],
        "cargo" => &[(
            r"^https://static\.crates\.io/crates/([^/]+)/([^/]+)/.+\.crate",
            "{proxy}/$1/$2/download",
        )],
        "openvsx" => &[(
            r"^https://open-vsx\.org/api/([^/]+)/([^/]+)/([^/]+)/file/.+\.vsix$",
            "{proxy}/$1.$2/$3/vsix",
        )],
        _ => &[],
    }
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

/// Escape the regex metacharacters that occur in URLs. `.` is the one that
/// actually matters (`nodejs.org` would otherwise match `nodejsXorg`), but the
/// rest are escaped too so a query string or a `+` in a path cannot change the
/// pattern's meaning.
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if r".+*?()[]{}^$|\".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

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

    let mut any = false;
    let mut skipped: Vec<&str> = Vec::new();
    for reg in regs {
        let rules = reg.mise_url_replacements(server_url);
        if rules.is_empty() {
            if !skipped.contains(&reg.registry_type.as_str()) {
                skipped.push(&reg.registry_type);
            }
            continue;
        }
        any = true;
        body.push_str(&format!("\n# {} ({})\n", reg.name, reg.registry_type));
        for (pattern, replacement) in rules {
            body.push_str(&format!(
                "{} = {}\n",
                toml_quote(&format!("regex:{pattern}")),
                toml_quote(&replacement)
            ));
        }
    }

    if !any {
        body.push_str("# (nothing mise downloads itself — see the note below)\n");
    }
    if catch_all {
        // The proxy's own host, first. The catch-all matches *any* https URL,
        // BatleHub's included, so an operator who has already pointed a
        // backend at the proxy — `NODEJS_ORG_MIRROR`, a `[settings]` entry, a
        // hand-written rule below — would have that URL rewritten into the
        // `501` sink and lose a registry that was working. Matching stops at
        // the first hit, so one identity rule ahead of the catch-all is the
        // whole fix (RFC 0008 §13, *Coverage, stated*).
        if let Some((pattern, replacement)) = mise_proxy_identity_rule(server_url) {
            body.push('\n');
            for line in wrap_comment(
                "This server's own URLs are left alone. The catch-all below matches every \
                 https host, so without this line a backend already pointed at BatleHub \
                 would be rewritten into the unmirrored sink.",
            ) {
                body.push_str(&format!("# {line}\n"));
            }
            body.push_str(&format!(
                "{} = {}\n",
                toml_quote(&format!("regex:{pattern}")),
                toml_quote(&replacement)
            ));
        }
        let (pattern, replacement) = mise_catch_all_rule(server_url);
        body.push('\n');
        for line in wrap_comment(
            "Air gap (RFC 0008): anything no rule above matched lands here. Nothing is \
             fetched — the proxy answers 501, names the host and records it, so an \
             unmirrored host is a line in the console instead of a connect timeout. \
             This rule must stay last: mise stops at the first match.",
        ) {
            body.push_str(&format!("# {line}\n"));
        }
        body.push_str(&format!(
            "{} = {}\n",
            toml_quote(&format!("regex:{pattern}")),
            toml_quote(&replacement)
        ));
    }
    push_mise_skipped_note(&mut body, &skipped);

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
pub fn render_toml(regs: &[SuggestedRegistry]) -> String {
    let mut out = String::new();
    out.push_str("# Generated by `batlehub-cli registry suggest`.\n");
    out.push_str("# Review before applying — names are suggestions, and RBAC below is a\n");
    out.push_str("# permissive starting point.\n");
    for reg in regs {
        out.push('\n');
        push_registry_block(&mut out, reg);
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
    if !reg.path_allow.is_empty() {
        out.push_str("path_allow = [\n");
        for p in &reg.path_allow {
            out.push_str(&format!("  \"{p}\",\n"));
        }
        out.push_str("]\n");
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
            "https://nodejs.org/dist/v24.18.0/node-v24.18.0-linux-x64.tar.gz",
            "mise.lock: node",
        )
        .unwrap();
        assert_eq!(reg.registry_type, "generic");
        assert_eq!(reg.name, "node-dist");
        assert_eq!(reg.upstreams, vec!["https://nodejs.org/dist"]);
        assert_eq!(reg.path_allow, vec!["v*/**"]);
        assert_eq!(
            reg.client_env,
            vec![("NODEJS_ORG_MIRROR".to_owned(), "{proxy}".to_owned())]
        );
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
        assert_eq!(
            reg.path_allow,
            vec!["claude-code-releases/2.1.217/linux-x64/claude"]
        );
        assert!(
            reg.note.is_some(),
            "unknown hosts must carry the widen-me caveat"
        );
    }

    #[test]
    fn urlless_backends_map_by_prefix() {
        let reg = registry_for_backend("cargo:cargo-audit", "mise.lock: cargo-audit").unwrap();
        assert_eq!(reg.registry_type, "cargo");
        let reg = registry_for_backend("pipx:semgrep", "mise.lock: semgrep").unwrap();
        assert_eq!(reg.registry_type, "pypi");
        let reg = registry_for_backend("aqua:owner/tool", "mise.lock: tool").unwrap();
        assert_eq!(reg.registry_type, "github");
        // core:rust ships no platform URL in the lock, so the backend must carry it.
        let reg = registry_for_backend("core:rust", "mise.lock: rust").unwrap();
        assert_eq!(reg.name, "rust-dist");
        assert_eq!(reg.registry_type, "generic");
        assert!(registry_for_backend("core:unknown-thing", "x").is_none());
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
        for expected in [
            "github",
            "cargo",
            "pypi",
            "node-dist",
            "rust-dist",
            "helm-bin",
        ] {
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
        assert!(got.contains(&"node-dist"), "{got:?}");
        assert!(got.contains(&"rust-dist"), "{got:?}");
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
        assert_eq!(names(&regs), vec!["node-dist"]);
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
            [(
                "NVM_NODEJS_ORG_MIRROR".to_owned(),
                "https://hub.example.com/proxy/node/nodedist".to_owned()
            )]
        );

        let toml = render_toml(&regs);
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
        let toml = render_toml(&regs);
        toml::from_str::<toml::Value>(&toml).expect("the rendered block is valid TOML");
        assert!(toml.contains("[registries.cache]"), "{toml}");
    }

    #[test]
    fn rendered_toml_is_parseable_and_passes_the_generic_allowlist_rule() {
        let mut acc = Accumulator::default();
        collect_from_mise_lock(SAMPLE_LOCK, &mut acc);
        let regs = acc.finish();
        let rendered = render_toml(&regs);

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
                "export NODEJS_ORG_MIRROR=\"https://hub.example.com/proxy/node-dist/generic\""
            ),
            "{env}"
        );
        assert!(
            env.contains("export RUSTUP_UPDATE_ROOT=\"https://hub.example.com/proxy/rust-dist/generic/rustup\""),
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
        let suggested = registry_for_url("https://nodejs.org/dist/v1/x.tar.gz", "s").unwrap();
        assert!(suggested
            .proxy_url("https://hub.example.com")
            .ends_with("/proxy/node-dist/generic"));
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
        let generic = registry_for_url("https://nodejs.org/dist/v1/x.tar.gz", "s").unwrap();
        assert_eq!(
            generic.proxy_url("https://hub.example.com"),
            "https://hub.example.com/proxy/node-dist/generic"
        );
        // GitHub routes as /proxy/{name}/{owner}/{repo}/… — a `/github/` segment
        // here would 404 every rewritten URL.
        let github = registry_for_backend("aqua:x/y", "s").unwrap();
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
            "https://nodejs.org/dist/v24.18.0/node-v24.18.0-linux-x64.tar.gz",
            "mise.lock: node",
        )
        .unwrap();
        let rules = reg.mise_url_replacements("https://hub.example.com");
        assert_eq!(
            rules,
            vec![(
                r"^https://nodejs\.org/dist/(.+)".to_owned(),
                "https://hub.example.com/proxy/node-dist/generic/$1".to_owned()
            )]
        );
    }

    #[test]
    fn github_mise_rules_cover_the_api_asset_path_aqua_actually_uses() {
        let reg = registry_for_backend("aqua:x/y", "mise.lock: y").unwrap();
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

    #[test]
    fn subprocess_driven_types_get_no_mise_rule() {
        let reg = registry_for_backend("pipx:semgrep", "mise.lock: semgrep").unwrap();
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
            Some("https://hub.example.com/proxy/node-dist/generic/$1"),
            "{rendered}"
        );
        assert!(repl.contains_key(r"regex:^https://api\.github\.com/repos/(.+)"));
        // pypi came from `pipx:semgrep` and cannot be rewritten — it must be
        // called out rather than silently missing.
        assert!(rendered.contains("Not rewritable here: pypi"), "{rendered}");
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
            Some("https://hub.example.com/proxy/node-dist/generic/$1")
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
            note: None,
        }];
        let env = render_client_env(&regs, "https://hub.example.com");
        assert!(
            env.contains("https://hub.example.com/proxy/helm-bin/generic/<path>"),
            "{env}"
        );
    }
}
