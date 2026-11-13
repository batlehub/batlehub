//! RFC 0045 — the mise wire table.
//!
//! One table of the URLs mise fetches itself, keyed on the registry kind that
//! serves them, and one *selection* deciding which registry of an instance
//! wires which of them. The CLI renders it as mise `url_replacements` (the
//! classic block, `client setup`); the server will route on it (phase 2). Both
//! call [`select`], so the same config cannot be wired two ways.
//!
//! Pure: no I/O, nothing resolved or dialled. The overlay's archive rule is
//! deliberately absent — it is added by the wire only, so a classic block
//! never prints a rule for a route that does not exist.

use std::collections::BTreeMap;

use regex::Regex;

use crate::entities::RegistryKind;

/// A `(regex, replacement)` pair: single-backslash regex escapes, `{proxy}`
/// for the registry's proxy base and `$N` for the captures.
pub type Rule = (&'static str, &'static str);

/// The curated rules of each kind whose URLs mise fetches itself.
///
/// GitHub needs six because mise reaches it through several URL shapes; the
/// `api.github.com/repos/…` rule is the load-bearing one, since aqua and ubi
/// resolve *and* download assets through the API (`/releases/assets/{id}`).
pub fn typed_rules(kind: RegistryKind) -> &'static [Rule] {
    match kind {
        RegistryKind::Github => &[
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
        RegistryKind::Forgejo => &[
            // API: release listings and tag metadata.
            (r"^https://codeberg\.org/api/v1/repos/(.+)", "{proxy}/$1"),
            // The asset itself, by attachment uuid. mise's `forgejo:` backend
            // builds this URL from its own configured api_url and never reads
            // the `browser_download_url` the rule below covers, so without this
            // one the checksum sibling comes here and the binary does not.
            (
                r"^https://codeberg\.org/attachments/(.+)",
                "{proxy}/attachments/$1",
            ),
            // Release asset binaries, for the clients that do follow the
            // document they were served.
            (
                r"^https://codeberg\.org/([^/]+)/([^/]+)/releases/download/([^/]+)/(.+)",
                "{proxy}/$1/$2/releases/download/$3/$4",
            ),
        ],
        // mise's `gitlab:` backend, sent at the typed `{project}/-/releases`
        // route, whose document repoints every asset at the instance — not at
        // the `api/v4` passthrough, which relays gitlab.com's own URLs. The
        // backend percent-encodes the project, so `$1` is one segment.
        RegistryKind::Gitlab => &[(
            r"^https://gitlab\.com/api/v4/projects/([^/]+)/releases(.*)",
            "{proxy}/$1/-/releases$2",
        )],
        // mise's versions host (RFC 0043): its whole surface under one rule —
        // the lists, the install beacon and the GitHub mirror the kind refuses.
        RegistryKind::MiseVersions => &[(
            r"^https://mise-versions\.jdx\.dev/(.+)",
            "{proxy}/mise-versions/$1",
        )],
        RegistryKind::Npm => &[(r"^https://registry\.npmjs\.org/(.+)", "{proxy}/$1")],
        // mise's `core:go` downloads `{go.download_mirror}/go{v}.{os}-{arch}.{ext}`
        // and its `.sha256` (`go.rs:126-128`, `:384-387`); the default mirror is
        // `https://dl.google.com/go`. Its listing is the versions host, or git.
        RegistryKind::Godist => &[(r"^https://dl\.google\.com/go/(.+)", "{proxy}/$1")],
        RegistryKind::Cargo => &[(
            r"^https://static\.crates\.io/crates/([^/]+)/([^/]+)/.+\.crate",
            "{proxy}/$1/$2/download",
        )],
        RegistryKind::Openvsx => &[(
            r"^https://open-vsx\.org/api/([^/]+)/([^/]+)/([^/]+)/file/.+\.vsix$",
            "{proxy}/$1.$2/$3/vsix",
        )],
        // mise's `conda:` backend: rattler fetches repodata and packages from
        // `conda.channel` (default `conda-forge`) and sends every request
        // through `url_replacements`. A registry is one channel, so the rule
        // is anchored on conda-forge's root and the subdir path carries over.
        // shortcut: only the default channel is routed; a `channel = "bioconda"`
        // tool goes direct until the rule is derived from the registry's upstream.
        RegistryKind::Conda => &[(
            r"^https://conda\.anaconda\.org/conda-forge/(.+)",
            "{proxy}/$1",
        )],
        _ => &[],
    }
}

/// The type segment a kind's proxy base carries after `/proxy/{name}`.
///
/// The path-addressed kinds route under it, `helm` because `…/helm` is the
/// repository URL `helm repo add` takes (RFC 0029 §4.2), `galaxy` because its
/// whole surface hangs off `…/galaxy/api/` (RFC 0031 §4.2).
pub fn proxy_segment(kind: RegistryKind) -> Option<&'static str> {
    (kind.is_path_addressed()
        || matches!(
            kind,
            RegistryKind::Nodedist
                | RegistryKind::Godist
                | RegistryKind::Sdkman
                | RegistryKind::Galaxy
                | RegistryKind::Helm
        ))
    .then(|| kind.as_str())
}

/// `/proxy/{name}` plus the kind's type segment: the base every rule's
/// `{proxy}` resolves to, shared by the rewrite rules and a plan's paths.
pub fn proxy_base_path(registry_name: &str, kind: RegistryKind) -> String {
    match proxy_segment(kind) {
        Some(seg) => format!("/proxy/{registry_name}/{seg}"),
        None => format!("/proxy/{registry_name}"),
    }
}

/// Escape the regex metacharacters that occur in URLs. `.` is the one that
/// actually matters (`nodejs.org` would otherwise match `nodejsXorg`), but the
/// rest are escaped too so a query string or a `+` in a path cannot change the
/// pattern's meaning.
pub fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if r".+*?()[]{}^$|\".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// What a rule wires: the host (with its port, when it has one) and the
/// literal path prefix before the first regex construct.
///
/// Every pattern is `^https://` followed by an escaped host, so this reads the
/// literal text back out instead of keeping a second table that could drift.
/// `None` for a pattern of any other shape.
pub fn wired_prefix(pattern: &str) -> Option<(String, String)> {
    let rest = pattern
        .strip_prefix("^https://")
        .or_else(|| pattern.strip_prefix("^http://"))?;
    let mut literal = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => literal.push(chars.next()?),
            '(' | '[' | '.' | '+' | '*' | '?' | '^' | '$' | '|' | '{' => break,
            _ => literal.push(c),
        }
    }
    let (host, prefix) = literal.split_once('/')?;
    // A prefix ends at its last `/`: `releases/download` stops at `releases/`,
    // which is what a request path is matched against segment-wise.
    let prefix = match prefix.rfind('/') {
        Some(i) => &prefix[..=i],
        None => "",
    };
    (!host.is_empty()).then(|| (host.to_ascii_lowercase(), prefix.to_owned()))
}

/// One registry, as the selection reads it — the CLI fills it from the
/// registry list, the server from its config.
#[derive(Debug, Clone)]
pub struct WireRegistry<'a> {
    pub name: &'a str,
    pub kind: RegistryKind,
    /// The first upstream, `None` when the server does not say.
    pub upstream: Option<&'a str>,
    /// Every upstream, for `generic` (one rule each).
    pub upstreams: &'a [String],
    /// `false` for a host-only registry (RFC 0001): it contributes nothing.
    pub path_routing: bool,
}

/// Who calls the selection: the two differ on one point only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The mise block a user writes: a forgejo registry in front of a
    /// self-hosted forge is skipped, its rules being codeberg's.
    Classic,
    /// The server's wire: the same forgejo rules are rebased onto the
    /// registry's own upstream, since the wire knows which forge it fronts.
    Wire,
}

/// One rule the selection keeps, with `{proxy}` still unresolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedRule {
    pub registry: String,
    pub kind: RegistryKind,
    pub pattern: String,
    pub replacement: String,
    pub host: String,
    pub prefix: String,
}

/// The rules kept, in registry then rule order, and why the others were not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub rules: Vec<SelectedRule>,
    pub skipped: Vec<String>,
}

const CONDA_FORGE: &str = "https://conda.anaconda.org/conda-forge";
const CODEBERG: &str = "https://codeberg.org";

/// The upstream of `r` when the server does not name one.
fn unnamed(kind: RegistryKind, r: &WireRegistry<'_>) -> String {
    format!(
        "{kind} registry {} (the server does not say what it proxies: that needs \
         `upstreams:read` granted to `*` on it)",
        r.name
    )
}

/// The one shape of URL a rule is built from: ASCII, no control, space or
/// quoting character, `http(s)`, no credentials, query or fragment. The string
/// is checked, not only what a parser makes of it — a parser drops a `\n` a
/// config file would read as a second setting.
pub fn is_plain_http_url(u: &str) -> bool {
    // Quoting and substitution for the shell, TOML, YAML and XML; `,` and `|`,
    // which separate proxies in GOPROXY (a URL parser reads
    // `https://hub/p,https://evil` as one URL on `hub`, go as two).
    const UNSAFE: &str = "\"'\\<>`$;|&{}^,";
    u.is_ascii()
        && !u
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || UNSAFE.contains(c))
        && url::Url::parse(u).is_ok_and(|p| {
            matches!(p.scheme(), "http" | "https")
                && p.username().is_empty()
                && p.password().is_none()
                && p.query().is_none()
                && p.fragment().is_none()
        })
}

/// An upstream as it goes into a rule, without its final `/`.
pub fn check_upstream(u: &str) -> Result<&str, String> {
    if is_plain_http_url(u) {
        Ok(u.trim_end_matches('/'))
    } else {
        Err(format!("its upstream {u:?} is not a plain http(s) URL"))
    }
}

/// Lower-cased host of `url`, when it parses.
pub fn url_host(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()?
        .host_str()
        .map(str::to_ascii_lowercase)
}

/// Why `r`'s typed rules must not be written: they rewrite a fixed public host
/// (`codeberg.org`, `api.github.com`, …) onto the registry, which is only
/// right when the registry proxies that host. An unknown upstream (an older
/// server) is not assumed public.
pub fn foreign_upstream(kind: RegistryKind, r: &WireRegistry<'_>) -> Option<String> {
    // A conda registry is one *channel*, and the rule is conda-forge's: the
    // host alone would let a bioconda mirror take over every conda-forge read.
    if kind == RegistryKind::Conda {
        return match r.upstream.map(|u| u.trim_end_matches('/')) {
            Some(CONDA_FORGE) => None,
            Some(u) => Some(format!(
                "conda registry {} (it proxies {u}, not {CONDA_FORGE})",
                r.name
            )),
            None => Some(unnamed(kind, r)),
        };
    }
    let public = match kind {
        RegistryKind::Github => "api.github.com",
        RegistryKind::Forgejo => "codeberg.org",
        RegistryKind::Gitlab => "gitlab.com",
        RegistryKind::Npm => "registry.npmjs.org",
        RegistryKind::Cargo => "crates.io",
        RegistryKind::Openvsx => "open-vsx.org",
        _ => return None,
    };
    match r.upstream.and_then(url_host) {
        Some(h) if h == public => None,
        Some(h) => Some(format!(
            "{kind} registry {} (it proxies {h}, not {public})",
            r.name
        )),
        None => Some(unnamed(kind, r)),
    }
}

/// The rules of one registry before claims are settled, or why it has none.
fn registry_rules(r: &WireRegistry<'_>, mode: Mode) -> Result<Vec<(String, String)>, Vec<String>> {
    if r.kind == RegistryKind::Generic {
        if r.upstreams.is_empty() {
            return Err(vec![format!(
                "generic registry {} (the server does not say what it mirrors: \
                 that needs `upstreams:read` granted to `*` on it)",
                r.name
            )]);
        }
        // A 1:1 path mirror: each upstream maps onto it. A bad one is skipped
        // alone, so the rules list and the skip list are both returned through
        // `Ok` — see `select`.
        return Ok(r
            .upstreams
            .iter()
            .map(|u| match check_upstream(u) {
                Ok(u) => (
                    format!("^{}/(.+)", regex_escape(u)),
                    "{proxy}/$1".to_owned(),
                ),
                Err(why) => (
                    String::new(),
                    format!("generic registry {} ({why})", r.name),
                ),
            })
            .collect());
    }
    let typed = typed_rules(r.kind);
    if typed.is_empty() {
        return Ok(Vec::new());
    }
    let owned = || {
        typed
            .iter()
            .map(|(p, rep)| ((*p).to_owned(), (*rep).to_owned()))
            .collect::<Vec<_>>()
    };
    match foreign_upstream(r.kind, r) {
        None => Ok(owned()),
        // The wire knows which forge a forgejo registry fronts: codeberg's
        // rules, rebased onto the registry's own upstream and path.
        Some(why) if mode == Mode::Wire && r.kind == RegistryKind::Forgejo => {
            let Some(Ok(up)) = r.upstream.map(check_upstream) else {
                return Err(vec![why]);
            };
            let from = format!("^{}", regex_escape(CODEBERG));
            let to = format!("^{}", regex_escape(up));
            Ok(owned()
                .into_iter()
                .map(|(p, rep)| (p.replacen(&from, &to, 1), rep))
                .collect())
        }
        Some(why) => Err(vec![why]),
    }
}

/// Which registry wires which rule, in declaration order. A later registry
/// whose rule repeats an earlier one's pattern is skipped and named — the
/// first declared wins, as mise's first-match would make it anyway.
pub fn select(registries: &[WireRegistry<'_>], mode: Mode) -> Selection {
    let mut out = Selection::default();
    let mut claimed: BTreeMap<String, String> = BTreeMap::new();
    for r in registries {
        if !r.path_routing {
            continue;
        }
        let rules = match registry_rules(r, mode) {
            Ok(rules) => rules,
            Err(why) => {
                out.skipped.extend(why);
                continue;
            }
        };
        for (pattern, replacement) in rules {
            // `registry_rules` hands a generic upstream it refused back as an
            // empty pattern carrying the reason.
            if pattern.is_empty() {
                out.skipped.push(replacement);
                continue;
            }
            if let Some(first) = claimed.get(&pattern) {
                out.skipped.push(if r.kind == RegistryKind::Generic {
                    let on = pattern
                        .strip_prefix('^')
                        .and_then(|p| p.strip_suffix("/(.+)"))
                        .map(|p| p.replace('\\', ""))
                        .unwrap_or_default();
                    format!(
                        "generic registry {} on {on} (registry {first} has it; \
                         --pick generic={} to swap)",
                        r.name, r.name
                    )
                } else {
                    format!("{} registry {} (registry {first} has it)", r.kind, r.name)
                });
                continue;
            }
            let Some((host, prefix)) = wired_prefix(&pattern) else {
                out.skipped.push(format!(
                    "{} registry {} (rule {pattern:?} names no host)",
                    r.kind, r.name
                ));
                continue;
            };
            claimed.insert(pattern.clone(), r.name.to_owned());
            out.rules.push(SelectedRule {
                registry: r.name.to_owned(),
                kind: r.kind,
                pattern,
                replacement,
                host,
                prefix,
            });
        }
    }
    out
}

/// Why the wire left something out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireWarning {
    /// `registry` wires `(host, prefix)` that `claimed_by`, declared earlier,
    /// already wires.
    PrefixClaimed {
        registry: String,
        claimed_by: String,
        host: String,
        prefix: String,
    },
    /// A registry, or one of its upstreams, contributes no rule; the reason.
    Skipped(String),
}

/// What the wire does with one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireOutcome {
    /// Rewritten in place to this path (always under `/proxy/`).
    Rewrite(String),
    /// `307` to the origin: wired, no typed rule, not a forge API.
    Bounce(String),
    /// The air-gap sink (always under `/_air-gap/unmirrored/`).
    Sink(String),
    /// `404 not_wired`.
    NotWired,
    /// `400`: the host or the path failed §7.3.
    Malformed,
}

#[derive(Debug)]
struct Pair {
    host: String,
    prefix: String,
    registry: String,
    proxy_base: String,
    /// A forge API prefix: an untyped path there is a gap in the table, never
    /// a bounce (an anonymous `403` from the forge is found slower than a
    /// `404` naming the missing rule).
    forge_api: bool,
    /// `http` for a plain-HTTP generic upstream; the wire path carries no
    /// scheme, so the rule is matched against the one it was written for.
    scheme: &'static str,
    rules: Vec<(Regex, String)>,
}

/// The wired `(host, prefix)` pairs of an instance, regexes compiled once.
#[derive(Debug, Default)]
pub struct WireTable {
    own_host: Option<String>,
    pairs: Vec<Pair>,
}

impl WireTable {
    /// The table for `registries`, in declaration order, and what it left out.
    /// `own_host` is this instance's host, which wires to itself.
    pub fn build(
        registries: &[WireRegistry<'_>],
        own_host: Option<&str>,
    ) -> (WireTable, Vec<WireWarning>) {
        let selection = select(registries, Mode::Wire);
        let mut warnings: Vec<WireWarning> = selection
            .skipped
            .into_iter()
            .map(WireWarning::Skipped)
            .collect();
        let mut pairs: Vec<Pair> = Vec::new();
        for rule in selection.rules {
            let re = match Regex::new(&rule.pattern) {
                Ok(re) => re,
                Err(e) => {
                    warnings.push(WireWarning::Skipped(format!(
                        "{} registry {} (rule {:?} does not compile: {e})",
                        rule.kind, rule.registry, rule.pattern
                    )));
                    continue;
                }
            };
            match pairs
                .iter_mut()
                .find(|p| p.host == rule.host && p.prefix == rule.prefix)
            {
                Some(p) if p.registry == rule.registry => p.rules.push((re, rule.replacement)),
                Some(p) => warnings.push(WireWarning::PrefixClaimed {
                    registry: rule.registry,
                    claimed_by: p.registry.clone(),
                    host: rule.host,
                    prefix: rule.prefix,
                }),
                None => {
                    let forge_api = matches!(
                        rule.kind,
                        RegistryKind::Github | RegistryKind::Forgejo | RegistryKind::Gitlab
                    ) && (rule.host.starts_with("api.")
                        || rule.prefix.contains("api/"));
                    pairs.push(Pair {
                        proxy_base: proxy_base_path(&rule.registry, rule.kind),
                        host: rule.host,
                        prefix: rule.prefix,
                        registry: rule.registry,
                        forge_api,
                        scheme: if rule.pattern.starts_with("^http://") {
                            "http"
                        } else {
                            "https"
                        },
                        rules: vec![(re, rule.replacement)],
                    });
                }
            }
        }
        let own_host = own_host.map(str::to_ascii_lowercase);
        (WireTable { own_host, pairs }, warnings)
    }

    /// Wire `mise.jdx.dev/registry/` to the `mise-versions` registry
    /// `registry`'s archive route (§15 Q17): present whenever the wire is
    /// enabled, whatever `overlay` says, so a client never strands on a cached
    /// archive. Other paths under the prefix are a plain bounce.
    #[must_use]
    pub fn with_registry_archive(mut self, registry: &str) -> Self {
        let rule = Regex::new(r"^https://mise\.jdx\.dev/registry/latest\.tar\.zst$")
            .expect("static pattern compiles");
        self.pairs.push(Pair {
            host: "mise.jdx.dev".to_owned(),
            prefix: "registry/".to_owned(),
            registry: registry.to_owned(),
            proxy_base: format!("/proxy/{registry}"),
            forge_api: false,
            scheme: "https",
            rules: vec![(
                rule,
                "{proxy}/mise-versions/_registry/latest.tar.zst".to_owned(),
            )],
        });
        self
    }

    /// Whether `registry` wires at least one prefix.
    pub fn wires_registry(&self, registry: &str) -> bool {
        self.pairs.iter().any(|p| p.registry == registry)
    }

    /// Every `(host, prefix, registry)` the table wires, in declaration order.
    pub fn wired(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.pairs
            .iter()
            .map(|p| (p.host.as_str(), p.prefix.as_str(), p.registry.as_str()))
    }

    /// Whether nothing is wired at all.
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty() && self.own_host.is_none()
    }

    /// [`Self::target`], where `request_host` (the host the request arrived
    /// on, without port) also counts as this instance's own host: the air-gap
    /// catch-all rule rewrites the instance's own URLs into the wire too.
    pub fn target_on(
        &self,
        request_host: &str,
        host: &str,
        rest: &str,
        air_gap: bool,
    ) -> WireOutcome {
        let name = host.split_once(':').map_or(host, |(n, _)| n);
        // A wired `host[:port]` wins: another service on the instance's host
        // (`127.0.0.1:8189` beside an instance on `:8156`) is not the instance.
        let wired = self.pairs.iter().any(|p| p.host == host);
        if !wired
            && !request_host.is_empty()
            && name == request_host
            && valid_host(host)
            && valid_rest(rest)
        {
            return own_host_target(rest);
        }
        self.target(host, rest, air_gap)
    }

    /// The wired `(host, prefix)` pairs, in declaration order.
    pub fn pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.pairs
            .iter()
            .map(|p| (p.host.as_str(), p.prefix.as_str()))
    }

    /// Where `https://{host}/{rest}` goes. `rest` excludes the query string,
    /// which the caller carries over unchanged.
    pub fn target(&self, host: &str, rest: &str, air_gap: bool) -> WireOutcome {
        if !valid_host(host) || !valid_rest(rest) {
            return WireOutcome::Malformed;
        }
        if self.own_host.as_deref() == Some(host) {
            return own_host_target(rest);
        }
        let sink = || WireOutcome::Sink(format!("/_air-gap/unmirrored/{host}/{rest}"));
        let Some(pair) = self
            .pairs
            .iter()
            .filter(|p| p.host == host && rest.starts_with(&p.prefix))
            .max_by_key(|p| p.prefix.len())
        else {
            return if air_gap {
                sink()
            } else {
                WireOutcome::NotWired
            };
        };
        let url = format!("{}://{host}/{rest}", pair.scheme);
        for (re, replacement) in &pair.rules {
            if let Some(caps) = re.captures(&url) {
                let mut out = replacement.replace("{proxy}", &pair.proxy_base);
                // `$9` before `$1`, so `$10` is never eaten by `$1`.
                for i in (1..=9).rev() {
                    let group = caps.get(i).map_or("", |m| m.as_str());
                    out = out.replace(&format!("${i}"), group);
                }
                return WireOutcome::Rewrite(out);
            }
        }
        if pair.forge_api {
            WireOutcome::NotWired
        } else if air_gap {
            sink()
        } else {
            WireOutcome::Bounce(url)
        }
    }
}

/// `proxy/{registry}/…` back to the instance itself; nothing else of it.
fn own_host_target(rest: &str) -> WireOutcome {
    let mut segs = rest.splitn(3, '/');
    match (segs.next(), segs.next(), segs.next()) {
        (Some("proxy"), Some(reg), Some(_)) if !reg.is_empty() => {
            WireOutcome::Rewrite(format!("/{rest}"))
        }
        _ => WireOutcome::NotWired,
    }
}

/// §7.3: lowercase `^[a-z0-9.-]{1,253}(:[0-9]{1,5})?$`.
fn valid_host(host: &str) -> bool {
    let (name, port) = match host.split_once(':') {
        Some((n, p)) => (n, Some(p)),
        None => (host, None),
    };
    (1..=253).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        && port.is_none_or(|p| (1..=5).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()))
}

/// §7.3: at most 4 KiB; no `.`/`..` or empty segment; no encoded `.`, `/` or
/// `\` at any `%25` depth; no control character, raw or encoded.
fn valid_rest(rest: &str) -> bool {
    if rest.len() > 4096
        || rest
            .chars()
            .any(|c| c.is_control() || c == '\\' || c == '?' || c == '#')
    {
        return false;
    }
    if rest
        .split('/')
        .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return false;
    }
    // Collapse every `%25` level, then look for what it would decode to.
    let mut lower = rest.to_ascii_lowercase();
    while lower.contains("%25") {
        lower = lower.replace("%25", "%");
    }
    let bytes = lower.as_bytes();
    !bytes.windows(3).any(|w| {
        w[0] == b'%'
            && matches!(
                (w[1], w[2]),
                (b'2', b'e' | b'f') | (b'5', b'c') | (b'0' | b'1', _) | (b'7', b'f')
            )
    })
}

/// A source registry, as an overlay tool's mise backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiseBackend {
    /// `github:{repo}` or `forgejo:{repo}`.
    pub backend: String,
    /// The forge's API root, for `forgejo:`.
    pub api_url: Option<String>,
}

/// Why a source cannot be written as a backend.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    #[error("a {0} registry cannot be an overlay source: only github and forgejo")]
    NotASource(RegistryKind),
    #[error("source upstream {0:?} must be scheme://host[:port][/path], without credentials, query or fragment")]
    Upstream(String),
    #[error("source repo {0:?} must be owner/name")]
    Repo(String),
}

/// The mise backend for an overlay tool sourced from a registry of `kind`.
///
/// The `api_url` is the upstream, never the instance: mise builds
/// `{api_url}/repos/{repo}/releases…`, which the wire then routes like any
/// other forge request.
pub fn source_backend(
    kind: RegistryKind,
    upstream: &str,
    repo: &str,
) -> Result<MiseBackend, SourceError> {
    let segment_ok = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    };
    if !repo
        .split_once('/')
        .is_some_and(|(o, n)| segment_ok(o) && segment_ok(n))
    {
        return Err(SourceError::Repo(repo.to_owned()));
    }
    match kind {
        RegistryKind::Github => Ok(MiseBackend {
            backend: format!("github:{repo}"),
            api_url: None,
        }),
        RegistryKind::Forgejo => {
            let up =
                check_upstream(upstream).map_err(|_| SourceError::Upstream(upstream.to_owned()))?;
            Ok(MiseBackend {
                backend: format!("forgejo:{repo}"),
                api_url: Some(format!("{up}/api/v1")),
            })
        }
        other => Err(SourceError::NotASource(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg<'a>(name: &'a str, kind: RegistryKind, upstream: Option<&'a str>) -> WireRegistry<'a> {
        WireRegistry {
            name,
            kind,
            upstream,
            upstreams: &[],
            path_routing: true,
        }
    }

    #[test]
    fn the_registry_archive_is_wired_to_the_mise_versions_registry() {
        let (t, _) = WireTable::build(&[], None);
        let t = t.with_registry_archive("mv");
        assert_eq!(
            t.target("mise.jdx.dev", "registry/latest.tar.zst", false),
            WireOutcome::Rewrite("/proxy/mv/mise-versions/_registry/latest.tar.zst".into())
        );
        // Another file under the prefix is a plain bounce; outside it, not wired.
        assert_eq!(
            t.target("mise.jdx.dev", "registry/other.tar.zst", false),
            WireOutcome::Bounce("https://mise.jdx.dev/registry/other.tar.zst".into())
        );
        assert_eq!(
            t.target("mise.jdx.dev", "install.sh", false),
            WireOutcome::NotWired
        );
    }

    #[test]
    fn a_wired_port_on_the_request_host_is_not_the_own_host() {
        let up = ["http://127.0.0.1:8189/a".to_owned()];
        let regs = [WireRegistry {
            name: "files",
            kind: RegistryKind::Generic,
            upstream: Some("http://127.0.0.1:8189/a"),
            upstreams: &up,
            path_routing: true,
        }];
        let (t, _) = WireTable::build(&regs, None);
        assert_eq!(
            t.target_on("127.0.0.1", "127.0.0.1:8189", "a/tool.tgz", false),
            WireOutcome::Rewrite("/proxy/files/generic/tool.tgz".into())
        );
        // The instance's own port is still the own host.
        assert_eq!(
            t.target_on(
                "127.0.0.1",
                "127.0.0.1:8156",
                "proxy/files/generic/x",
                false
            ),
            WireOutcome::Rewrite("/proxy/files/generic/x".into())
        );
    }

    #[test]
    fn the_request_host_is_the_own_host() {
        let (t, _) = WireTable::build(&[], None);
        assert_eq!(
            t.target_on(
                "hub.example.com",
                "hub.example.com:8443",
                "proxy/npm/lodash",
                false
            ),
            WireOutcome::Rewrite("/proxy/npm/lodash".into())
        );
        assert_eq!(
            t.target_on(
                "hub.example.com",
                "hub.example.com",
                "api/v1/admin/users",
                false
            ),
            WireOutcome::NotWired
        );
        assert_eq!(
            t.target_on("hub.example.com", "hub.example.com", "proxy/../api", false),
            WireOutcome::Malformed
        );
    }

    #[test]
    fn every_typed_rule_names_a_host_and_compiles() {
        for kind in RegistryKind::ALL {
            for (pattern, replacement) in typed_rules(*kind) {
                assert!(wired_prefix(pattern).is_some(), "{pattern}");
                assert!(Regex::new(pattern).is_ok(), "{pattern}");
                assert!(replacement.starts_with("{proxy}/"), "{replacement}");
            }
        }
    }

    #[test]
    fn the_prefix_is_the_literal_text_up_to_the_last_slash() {
        let p = |s: &str| wired_prefix(s).unwrap();
        assert_eq!(
            p(r"^https://api\.github\.com/repos/(.+)"),
            ("api.github.com".into(), "repos/".into())
        );
        assert_eq!(
            p(r"^https://github\.com/([^/]+)/x"),
            ("github.com".into(), String::new())
        );
        assert_eq!(
            p(r"^https://codeberg\.org/attachments/(.+)"),
            ("codeberg.org".into(), "attachments/".into())
        );
        assert_eq!(
            p(&format!(
                "^{}/(.+)",
                regex_escape("https://storage.googleapis.com/acme-bucket")
            )),
            ("storage.googleapis.com".into(), "acme-bucket/".into())
        );
        assert_eq!(
            p(&format!("^{}/(.+)", regex_escape("http://mirror.lan:8081"))),
            ("mirror.lan:8081".into(), String::new())
        );
    }

    #[test]
    fn the_archive_rule_is_not_in_the_shared_table() {
        for kind in RegistryKind::ALL {
            for (pattern, _) in typed_rules(*kind) {
                assert!(!pattern.contains("mise\\.jdx\\.dev"), "{pattern}");
            }
        }
    }

    #[test]
    fn a_foreign_typed_registry_is_skipped_with_its_reason() {
        let regs = [
            reg("gh", RegistryKind::Github, Some("https://api.github.com")),
            reg(
                "fj",
                RegistryKind::Forgejo,
                Some("https://git.corp.example"),
            ),
            reg(
                "bio",
                RegistryKind::Conda,
                Some("https://conda.anaconda.org/bioconda"),
            ),
            reg(
                "dflt",
                RegistryKind::Conda,
                Some("https://conda.anaconda.org"),
            ),
            reg("npm", RegistryKind::Npm, None),
        ];
        let s = select(&regs, Mode::Classic);
        assert!(s.rules.iter().all(|r| r.registry == "gh"), "{s:?}");
        assert_eq!(s.skipped.len(), 4, "{:?}", s.skipped);
        assert!(s.skipped[0].contains("proxies git.corp.example, not codeberg.org"));
        assert!(s.skipped[3].contains("does not say"));
    }

    #[test]
    fn the_wire_rebases_a_self_hosted_forgejo_onto_its_own_prefix() {
        let regs = [reg(
            "fj",
            RegistryKind::Forgejo,
            Some("https://git.corp.example/forge/"),
        )];
        let (table, warnings) = WireTable::build(&regs, None);
        assert!(warnings.is_empty(), "{warnings:?}");
        let pairs: Vec<_> = table.pairs().collect();
        assert!(
            pairs.contains(&("git.corp.example", "forge/api/v1/repos/")),
            "{pairs:?}"
        );
        assert_eq!(
            table.target("git.corp.example", "forge/api/v1/repos/o/r/releases", false),
            WireOutcome::Rewrite("/proxy/fj/o/r/releases".into())
        );
        assert_eq!(
            table.target("codeberg.org", "api/v1/repos/o/r", false),
            WireOutcome::NotWired
        );
    }

    #[test]
    fn generic_upstreams_wire_their_path_and_the_first_claim_wins() {
        let a = vec![
            "https://storage.googleapis.com/acme-bucket/".to_owned(),
            "ftp://nope".to_owned(),
        ];
        let b = vec!["https://storage.googleapis.com/acme-bucket".to_owned()];
        let regs = [
            WireRegistry {
                upstreams: &a,
                ..reg("ga", RegistryKind::Generic, None)
            },
            WireRegistry {
                upstreams: &b,
                ..reg("gb", RegistryKind::Generic, None)
            },
        ];
        let s = select(&regs, Mode::Classic);
        assert_eq!(s.rules.len(), 1);
        assert!(
            s.skipped[0].contains("not a plain http(s) URL"),
            "{:?}",
            s.skipped
        );
        assert!(
            s.skipped[1].contains("generic registry gb on https://storage.googleapis.com/acme-bucket (registry ga has it"),
            "{:?}",
            s.skipped
        );
        let (table, _) = WireTable::build(&regs, None);
        assert_eq!(
            table.target("storage.googleapis.com", "acme-bucket/v1/tool.tgz", false),
            WireOutcome::Rewrite("/proxy/ga/generic/v1/tool.tgz".into())
        );
        // Another bucket on the same host is not wired.
        assert_eq!(
            table.target("storage.googleapis.com", "other/x", false),
            WireOutcome::NotWired
        );
    }

    fn table() -> WireTable {
        let regs = [
            reg("gh", RegistryKind::Github, Some("https://api.github.com")),
            reg("mv", RegistryKind::MiseVersions, None),
            reg(
                "cf",
                RegistryKind::Conda,
                Some("https://conda.anaconda.org/conda-forge/"),
            ),
            WireRegistry {
                path_routing: false,
                ..reg("npm", RegistryKind::Npm, Some("https://registry.npmjs.org"))
            },
        ];
        let (t, w) = WireTable::build(&regs, Some("hub.example.com"));
        assert!(w.is_empty(), "{w:?}");
        t
    }

    #[test]
    fn each_outcome_of_the_behaviour_table() {
        let t = table();
        assert_eq!(
            t.target("github.com", "o/r/releases/download/v1/a.tgz", false),
            WireOutcome::Rewrite("/proxy/gh/o/r/releases/download/v1/a".to_owned() + ".tgz")
        );
        assert_eq!(
            t.target(
                "conda.anaconda.org",
                "conda-forge/linux-64/repodata.json",
                false
            ),
            WireOutcome::Rewrite("/proxy/cf/linux-64/repodata.json".into())
        );
        // Wired, no typed rule: bounce, or the sink in an air gap.
        assert_eq!(
            t.target("github.com", "o/r/blob/main/README.md", false),
            WireOutcome::Bounce("https://github.com/o/r/blob/main/README.md".into())
        );
        assert_eq!(
            t.target("github.com", "o/r/blob/main/README.md", true),
            WireOutcome::Sink("/_air-gap/unmirrored/github.com/o/r/blob/main/README.md".into())
        );
        // A forge API prefix never bounces.
        assert_eq!(
            t.target("api.github.com", "users/x", false),
            WireOutcome::NotWired
        );
        // Not wired at all.
        assert_eq!(t.target("example.org", "x", false), WireOutcome::NotWired);
        assert_eq!(
            t.target("example.org", "x", true),
            WireOutcome::Sink("/_air-gap/unmirrored/example.org/x".into())
        );
        // A host-only registry contributes nothing.
        assert_eq!(
            t.target("registry.npmjs.org", "left-pad", false),
            WireOutcome::NotWired
        );
        // The own host: back to itself, and only under `proxy/{registry}/`.
        assert_eq!(
            t.target("hub.example.com", "proxy/gh/o/r", false),
            WireOutcome::Rewrite("/proxy/gh/o/r".into())
        );
        assert_eq!(
            t.target("hub.example.com", "api/v1/me", false),
            WireOutcome::NotWired
        );
    }

    #[test]
    fn malformed_hosts_and_paths_are_refused_before_any_rule() {
        let t = table();
        for host in [
            "GitHub.com",
            "github.com:",
            "github.com:123456",
            "a b",
            "",
            "github.com/x",
        ] {
            assert_eq!(
                t.target(host, "o/r", false),
                WireOutcome::Malformed,
                "{host}"
            );
        }
        for rest in [
            "o/../r",
            "o/./r",
            "o//r",
            "",
            "o/r/",
            "%2e%2e/x",
            "%2E%2e/x",
            "a%2fb",
            "a%5Cb",
            "%252e%252e/x",
            "%25252F",
            "a%0D%0Ab",
            "a\\b",
            "a?b",
            "a#b",
            "a%7fb",
        ] {
            assert_eq!(
                t.target("github.com", rest, false),
                WireOutcome::Malformed,
                "{rest}"
            );
        }
        let long = "a/".repeat(2049) + "a";
        assert_eq!(t.target("github.com", &long, false), WireOutcome::Malformed);
        // Encoded `?` and `#` stay data.
        assert!(matches!(
            t.target("github.com", "o/r/blob/%3F%23", false),
            WireOutcome::Bounce(_)
        ));
    }

    #[test]
    fn a_port_is_part_of_the_wired_host() {
        let up = vec!["http://mirror.lan:8081".to_owned()];
        let regs = [WireRegistry {
            upstreams: &up,
            ..reg("m", RegistryKind::Generic, None)
        }];
        let (t, _) = WireTable::build(&regs, None);
        assert_eq!(
            t.target("mirror.lan:8081", "x/y", false),
            WireOutcome::Rewrite("/proxy/m/generic/x/y".into())
        );
        assert_eq!(
            t.target("mirror.lan:9999", "x/y", false),
            WireOutcome::NotWired
        );
        assert_eq!(t.target("mirror.lan", "x/y", false), WireOutcome::NotWired);
    }

    #[test]
    fn a_repeated_prefix_is_reported_not_wired_twice() {
        let regs = [
            reg("a", RegistryKind::Npm, Some("https://registry.npmjs.org")),
            reg("b", RegistryKind::Npm, Some("https://registry.npmjs.org")),
        ];
        let s = select(&regs, Mode::Wire);
        assert_eq!(s.rules.len(), 1);
        assert_eq!(s.skipped, ["npm registry b (registry a has it)"]);
    }

    #[test]
    fn source_backends() {
        assert_eq!(
            source_backend(RegistryKind::Github, "https://api.github.com", "o/r").unwrap(),
            MiseBackend {
                backend: "github:o/r".into(),
                api_url: None
            }
        );
        assert_eq!(
            source_backend(RegistryKind::Forgejo, "https://git.corp.example/", "o/r")
                .unwrap()
                .api_url
                .as_deref(),
            Some("https://git.corp.example/api/v1")
        );
        for bad in [
            "https://u:p@git.corp.example",
            "https://git.corp.example?x",
            "https://h#f",
        ] {
            assert!(matches!(
                source_backend(RegistryKind::Forgejo, bad, "o/r"),
                Err(SourceError::Upstream(_))
            ));
        }
        for repo in ["o", "o/r/x", "../r", "o/r\"", "o/"] {
            assert!(
                matches!(
                    source_backend(RegistryKind::Github, "https://api.github.com", repo),
                    Err(SourceError::Repo(_))
                ),
                "{repo}"
            );
        }
        assert!(matches!(
            source_backend(RegistryKind::Npm, "https://registry.npmjs.org", "o/r"),
            Err(SourceError::NotASource(_))
        ));
    }
}
