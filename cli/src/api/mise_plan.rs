//! `mise.lock` as a bill of materials (RFC 0008 §4.2).
//!
//! A lock file already records the exact URL and checksum of every tool, per
//! platform. Today the CLI turns that into *advice* — a
//! `[settings.url_replacements]` block. This turns it into a **plan**: every
//! download the locked tools imply, resolved onto the path it takes *through
//! BatleHub*, so a plan is a statement about this instance rather than about
//! a list of URLs.
//!
//! Four fields carry the argument:
//!
//! * `proxy_path` — the path this download takes through BatleHub. It is what
//!   `mise seed` fetches and what `mise export` reads the bytes from, and it
//!   is a path rather than a URL so a plan is not a statement about the
//!   machine that made it.
//! * `key` — the storage key it is *expected* to occupy. A best guess, and
//!   labelled as one: the real key is a function of the route, not of the
//!   URL, and only the server knows it. The export takes the key off the
//!   response's `X-BatleHub-Storage-Key` and uses this one only when talking
//!   to a server too old to send it (RFC 0008 §14.1).
//! * `unsupported` — the tools that will not work, named with the reason,
//!   *before* the bundle is built rather than at install time on a
//!   disconnected workstation.
//! * `unmirrored_hosts` — every host in the lock for which this server has no
//!   registry. It is the answer to "is my rewrite table complete?", which
//!   today has no answer at all.
//!
//! Planning is **offline**: it reads the lock and the server's registry list,
//! and resolves nothing over the network.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::suggest::{
    proxy_path_for, registry_kind_for_host, split_url, unsupported_backend_reason,
};

/// The plan format's own version, so a reader can refuse one it predates.
pub const PLAN_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MisePlan {
    pub plan_version: u32,
    pub generated_from: PlanSource,
    pub platforms: Vec<String>,
    pub entries: Vec<PlanEntry>,
    /// Tools that cannot come through BatleHub at all, with the reason.
    pub unsupported: Vec<UnsupportedTool>,
    /// Hosts in the lock that no configured registry mirrors.
    pub unmirrored_hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanSource {
    pub file: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanEntry {
    pub tool: String,
    pub version: String,
    pub platform: String,
    pub url: String,
    /// Which of an artifact's two addresses this entry names.
    #[serde(default, skip_serializing_if = "PlanAddress::is_download")]
    pub address: PlanAddress,
    /// The registry this download goes through, when one is configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<PlanRegistry>,
    /// The storage key it is expected to occupy, for a human reading the
    /// plan. Absent when no registry mirrors the host — there is no key to
    /// name, and `unmirrored_hosts` says so.
    ///
    /// **Not the identity.** A storage key is a function of the route rather
    /// than of the URL, so this is a guess that is right for the simple
    /// shapes and wrong for several real ones; the bundle uses what the
    /// server reports instead (RFC 0008 §14.1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The path this download takes through BatleHub — a path rather than a
    /// URL, so a plan is not a statement about the machine that made it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_path: Option<String>,
    /// The lock's own digest for these bytes, when it records one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// The two addresses the same bytes answer to.
///
/// A lock records both for a forge release asset: `url` is the browser
/// download URL, `url_api` the API's asset endpoint. They are one artifact —
/// same digest, same blob — reached by two paths, and a disconnected
/// instance has to answer whichever the client asks for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanAddress {
    #[default]
    Download,
    Api,
}

impl PlanAddress {
    /// Serialisation skips the common case, so a plan of ordinary downloads
    /// reads exactly as it did before this field existed.
    pub fn is_download(&self) -> bool {
        matches!(self, Self::Download)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRegistry {
    pub name: String,
    #[serde(rename = "type")]
    pub registry_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsupportedTool {
    pub tool: String,
    pub reason: String,
}

/// One registry as the server reports it, reduced to what planning needs.
#[derive(Debug, Clone)]
pub struct KnownRegistry {
    pub name: String,
    pub registry_type: String,
}

/// Build a plan from a lock file's text.
///
/// `platforms` narrows the lock's per-platform blocks; empty keeps every
/// platform the lock records. `known` is the server's registry list —
/// planning against an empty list still produces a plan, and reports every
/// host as unmirrored, which is the honest answer for a server that has no
/// registries yet.
pub fn plan_from_lock(
    file_name: &str,
    content: &str,
    platforms: &[String],
    known: &[KnownRegistry],
) -> Result<MisePlan> {
    let doc: toml::Value =
        toml::from_str(content).with_context(|| format!("{file_name} is not valid TOML"))?;
    let mut entries: Vec<PlanEntry> = Vec::new();
    let mut unsupported: Vec<UnsupportedTool> = Vec::new();
    let mut hosts: BTreeSet<String> = BTreeSet::new();
    let mut seen_platforms: BTreeSet<String> = BTreeSet::new();

    let tools = doc
        .get("tools")
        .and_then(|t| t.as_table())
        .cloned()
        .unwrap_or_default();

    for (tool_name, value) in &tools {
        // `[[tools.x]]` yields an array; a bare table is tolerated, as the
        // suggest path already tolerates it.
        let tables: Vec<&toml::Table> = match value {
            toml::Value::Table(t) => vec![t],
            toml::Value::Array(items) => items.iter().filter_map(|v| v.as_table()).collect(),
            _ => continue,
        };
        for table in tables {
            plan_one_tool(
                tool_name,
                table,
                platforms,
                &mut entries,
                &mut unsupported,
                &mut seen_platforms,
            );
        }
    }

    // A host is mirrored when some configured registry has the kind that
    // host maps to. Kind rather than name, because the plan is written
    // before anyone chooses names and two registries of the same kind serve
    // the same hosts.
    let configured: BTreeSet<&str> = known.iter().map(|r| r.registry_type.as_str()).collect();
    for entry in &mut entries {
        let Some((host, _)) = split_url(&entry.url) else {
            continue;
        };
        match registry_kind_for_host(host) {
            Some(kind) if configured.contains(kind) => {
                let registry =
                    known
                        .iter()
                        .find(|r| r.registry_type == kind)
                        .map(|r| PlanRegistry {
                            name: r.name.clone(),
                            registry_type: r.registry_type.clone(),
                        });
                entry.key = registry
                    .as_ref()
                    .map(|r| storage_key(&r.name, &entry.tool, &entry.version, &entry.url));
                entry.proxy_path = registry
                    .as_ref()
                    .and_then(|r| proxy_path_for(&entry.url, &r.name, &r.registry_type));
                entry.registry = registry;
            }
            _ => {
                hosts.insert(host.to_owned());
            }
        }
    }

    let mut platforms_out: Vec<String> = if platforms.is_empty() {
        seen_platforms.into_iter().collect()
    } else {
        platforms.to_vec()
    };
    platforms_out.sort();
    platforms_out.dedup();

    unsupported.sort_by(|a, b| a.tool.cmp(&b.tool));
    unsupported.dedup_by(|a, b| a.tool == b.tool);
    entries.sort_by(|a, b| {
        a.tool
            .cmp(&b.tool)
            .then(a.platform.cmp(&b.platform))
            .then(a.url.cmp(&b.url))
    });

    Ok(MisePlan {
        plan_version: PLAN_VERSION,
        generated_from: PlanSource {
            file: file_name.to_owned(),
            sha256: sha256_hex(content.as_bytes()),
        },
        platforms: platforms_out,
        entries,
        unsupported,
        unmirrored_hosts: hosts.into_iter().collect(),
    })
}

fn plan_one_tool(
    tool_name: &str,
    table: &toml::Table,
    platforms: &[String],
    entries: &mut Vec<PlanEntry>,
    unsupported: &mut Vec<UnsupportedTool>,
    seen_platforms: &mut BTreeSet<String>,
) {
    let version = table
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_owned();
    let backend = table
        .get("backend")
        .and_then(|b| b.as_str())
        .unwrap_or(tool_name);

    // Every per-platform block that carries a `url`. The lock nests them
    // under `platforms.<name>`, but older locks put them at the top level;
    // both shapes are read, because a plan that silently skipped a tool
    // would be a bundle missing it.
    let mut found = false;
    for (key, sub) in platform_tables(table) {
        let Some(url) = sub.get("url").and_then(|u| u.as_str()) else {
            continue;
        };
        found = true;
        seen_platforms.insert(key.clone());
        if !platforms.is_empty() && !platforms.iter().any(|p| platform_matches(&key, p)) {
            continue;
        }
        let sha256 = sub
            .get("checksum")
            .and_then(|c| c.as_str())
            .and_then(strip_sha256_prefix)
            .map(str::to_owned);
        let size = sub
            .get("size")
            .and_then(|s| s.as_integer())
            .map(|s| s as u64);
        entries.push(PlanEntry {
            tool: tool_name.to_owned(),
            version: version.clone(),
            platform: key.clone(),
            url: url.to_owned(),
            address: PlanAddress::Download,
            registry: None,
            key: None,
            proxy_path: None,
            sha256: sha256.clone(),
            size,
        });
        // `url_api` is the *same bytes at a second address*: the forge API's
        // own asset endpoint. mise fetches whichever the lock gives it, and
        // for the `aqua:`/`github:` backends that is usually this one — so a
        // bundle carrying only the browser download URL leaves the
        // disconnected install one `503` short. Both are planned; the bundle
        // is content-addressed, so the two keys share one blob and the
        // second address costs nothing but a manifest row.
        if let Some(api) = sub.get("url_api").and_then(|u| u.as_str()) {
            entries.push(PlanEntry {
                tool: tool_name.to_owned(),
                version: version.clone(),
                platform: key,
                url: api.to_owned(),
                address: PlanAddress::Api,
                registry: None,
                key: None,
                proxy_path: None,
                sha256,
                size,
            });
        }
    }

    if !found {
        // No download at all: either a source backend BatleHub does mirror
        // (cargo, npm — mise shells out to the ecosystem's own tool, which
        // the client-env block points at the proxy), or one it cannot.
        if let Some(reason) = unsupported_backend_reason(backend) {
            unsupported.push(UnsupportedTool {
                tool: tool_name.to_owned(),
                reason,
            });
        }
    }
}

/// The `platforms.*` sub-tables, or the top-level ones on an older lock.
///
/// Two spellings reach here and they are not the same TOML. `mise lock`
/// writes the *quoted* form — `[tools.node."platforms.linux-x64"]` — which
/// parses as one key literally named `platforms.linux-x64` on the tool
/// table, not as a `platforms` table with a `linux-x64` child. Reading it
/// naively names the platform `platforms.linux-x64`, and then
/// `--platform linux-x64` matches nothing and plans an empty bundle for a
/// lock full of tools. The prefix is stripped here, once, so both spellings
/// answer to the name an operator types.
fn platform_tables(table: &toml::Table) -> Vec<(String, toml::Table)> {
    if let Some(platforms) = table.get("platforms").and_then(|p| p.as_table()) {
        return platforms
            .iter()
            .filter_map(|(k, v)| v.as_table().map(|t| (k.clone(), t.clone())))
            .collect();
    }
    table
        .iter()
        .filter_map(|(k, v)| {
            v.as_table().map(|t| {
                (
                    k.strip_prefix("platforms.").unwrap_or(k).to_owned(),
                    t.clone(),
                )
            })
        })
        .collect()
}

/// The platform key of the machine the plan is being made on (RFC 0008 §13
/// decision 1).
///
/// mise spells a platform `<os>-<arch>`, and the default is this host's —
/// the same shape as RFC 0010 decision 11's `warm_platforms`, and for the
/// same reason: planning every platform a lock happens to record builds a
/// bundle several times the size of the one the estate asked for.
pub fn host_platform() -> String {
    // mise's os names are Rust's: `linux`, `macos`, `windows`.
    let os = std::env::consts::OS;
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("{os}-{arch}")
}

/// Whether a lock's platform key names the same platform as `wanted`.
///
/// `darwin-arm64` and `macos-arm64` are one platform under two spellings —
/// mise has used both — and an operator who types the one their lock does
/// not use should get their tools, not an empty plan.
fn platform_matches(key: &str, wanted: &str) -> bool {
    fn canonical(p: &str) -> String {
        p.replacen("darwin-", "macos-", 1)
    }
    canonical(key) == canonical(wanted)
}

fn strip_sha256_prefix(raw: &str) -> Option<&str> {
    let hex = raw.strip_prefix("sha256:").unwrap_or(raw);
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hex)
}

/// The storage key the artifact will occupy.
///
/// Derived the same way the proxy derives it — `{registry}/{name}/{version}`
/// plus the file — so a plan names what `mise seed` warms and what a bundle
/// entry carries. The file name comes from the URL's last segment, which is
/// what every one of these downloads is addressed by.
fn storage_key(registry: &str, tool: &str, version: &str, url: &str) -> String {
    let file = url
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("artifact");
    let name = tool.split_once(':').map(|(_, n)| n).unwrap_or(tool);
    format!("{registry}/{name}/{version}/{file}")
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// mise's own release asset for one platform.
///
/// The naming is mise's: `mise-v<version>-<os>-<arch>.tar.gz` on the
/// `jdx/mise` GitHub release, which is what `mise self-update` downloads.
fn mise_asset_url(version: &str, platform: &str) -> String {
    format!(
        "https://github.com/jdx/mise/releases/download/v{version}/mise-v{version}-{platform}.tar.gz"
    )
}

/// Add `github:jdx/mise@<version>` to a plan (RFC 0008 §13 decision 2).
///
/// One entry per platform the plan already covers, so `--platform` decides
/// this the same way it decides everything else, and a plan for one host
/// does not quietly carry three mise binaries.
pub fn add_mise_itself(plan: &mut MisePlan, version: &str, known: &[KnownRegistry]) {
    let github = known.iter().find(|r| r.registry_type == "github");
    let platforms: Vec<String> = if plan.platforms.is_empty() {
        vec![host_platform()]
    } else {
        plan.platforms.clone()
    };
    for platform in platforms {
        let url = mise_asset_url(version, &platform);
        if plan.entries.iter().any(|e| e.url == url) {
            continue;
        }
        let registry = github.map(|r| PlanRegistry {
            name: r.name.clone(),
            registry_type: r.registry_type.clone(),
        });
        if registry.is_none() && !plan.unmirrored_hosts.iter().any(|h| h == "github.com") {
            plan.unmirrored_hosts.push("github.com".to_owned());
            plan.unmirrored_hosts.sort();
        }
        plan.entries.push(PlanEntry {
            tool: "github:jdx/mise".to_owned(),
            version: version.to_owned(),
            platform,
            key: registry
                .as_ref()
                .map(|r| storage_key(&r.name, "jdx/mise", version, &url)),
            proxy_path: registry
                .as_ref()
                .and_then(|r| proxy_path_for(&url, &r.name, &r.registry_type)),
            registry,
            url,
            address: PlanAddress::Download,
            // The lock does not record mise itself, so there is no checksum
            // to compare against — `seed` says so rather than claiming a
            // match nobody made.
            sha256: None,
            size: None,
        });
    }
    plan.entries.sort_by(|a, b| {
        a.tool
            .cmp(&b.tool)
            .then(a.platform.cmp(&b.platform))
            .then(a.url.cmp(&b.url))
    });
}

/// Read a lock file and plan it.
pub fn plan_from_path(
    path: &std::path::Path,
    platforms: &[String],
    known: &[KnownRegistry],
) -> Result<MisePlan> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    plan_from_lock(&name, &content, platforms, known)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = r#"
[tools."aqua:EmbarkStudios/cargo-deny"]
version = "0.18.2"
backend = "aqua:EmbarkStudios/cargo-deny"
[tools."aqua:EmbarkStudios/cargo-deny".platforms.linux-x64]
url = "https://github.com/EmbarkStudios/cargo-deny/releases/download/0.18.2/cargo-deny-0.18.2-x86_64-unknown-linux-musl.tar.gz"
checksum = "sha256:4f0c000000000000000000000000000000000000000000000000000000000000"
size = 5439201
[tools."aqua:EmbarkStudios/cargo-deny".platforms.darwin-arm64]
url = "https://github.com/EmbarkStudios/cargo-deny/releases/download/0.18.2/cargo-deny-0.18.2-aarch64-apple-darwin.tar.gz"

[tools."asdf:mise-plugins/mise-postgres"]
version = "16.2"
backend = "asdf:mise-plugins/mise-postgres"

[tools.sonar]
version = "5.0"
[tools.sonar.platforms.linux-x64]
url = "https://binaries.sonarsource.com/Distribution/sonar-scanner/sonar-scanner-5.0.zip"
"#;

    fn known() -> Vec<KnownRegistry> {
        vec![KnownRegistry {
            name: "gh".into(),
            registry_type: "github".into(),
        }]
    }

    #[test]
    fn the_plan_names_a_storage_key_for_every_mirrored_download() {
        let plan = plan_from_lock("mise.lock", LOCK, &[], &known()).unwrap();
        assert_eq!(plan.plan_version, PLAN_VERSION);
        assert_eq!(plan.generated_from.file, "mise.lock");
        assert_eq!(plan.generated_from.sha256.len(), 64);

        let deny: Vec<&PlanEntry> = plan
            .entries
            .iter()
            .filter(|e| e.tool.contains("cargo-deny"))
            .collect();
        assert_eq!(deny.len(), 2, "one per platform");
        let linux = deny.iter().find(|e| e.platform == "linux-x64").unwrap();
        assert_eq!(linux.registry.as_ref().unwrap().name, "gh");
        assert_eq!(
            linux.key.as_deref(),
            Some("gh/EmbarkStudios/cargo-deny/0.18.2/cargo-deny-0.18.2-x86_64-unknown-linux-musl.tar.gz")
        );
        assert_eq!(
            linux.proxy_path.as_deref(),
            Some("/proxy/gh/EmbarkStudios/cargo-deny/releases/download/0.18.2/cargo-deny-0.18.2-x86_64-unknown-linux-musl.tar.gz"),
            "the seed fetches exactly what mise would fetch"
        );
        assert_eq!(linux.sha256.as_deref().unwrap().len(), 64);
        assert_eq!(linux.size, Some(5439201));
        // The lock's platform names are what the plan reports.
        assert_eq!(plan.platforms, vec!["darwin-arm64", "linux-x64"]);
    }

    #[test]
    fn a_host_with_no_registry_is_named_rather_than_silently_dropped() {
        let plan = plan_from_lock("mise.lock", LOCK, &[], &known()).unwrap();
        assert_eq!(plan.unmirrored_hosts, vec!["binaries.sonarsource.com"]);
        let sonar = plan.entries.iter().find(|e| e.tool == "sonar").unwrap();
        assert!(
            sonar.key.is_none() && sonar.registry.is_none(),
            "there is no key to name for a host nothing mirrors"
        );
    }

    #[test]
    fn a_git_backend_is_unsupported_and_says_why_before_the_bundle_is_built() {
        let plan = plan_from_lock("mise.lock", LOCK, &[], &known()).unwrap();
        assert_eq!(plan.unsupported.len(), 1);
        assert!(plan.unsupported[0].tool.contains("mise-postgres"));
        assert!(
            plan.unsupported[0].reason.contains("git"),
            "the message names the backend, not the symptom: {}",
            plan.unsupported[0].reason
        );
    }

    #[test]
    fn a_platform_filter_narrows_the_entries_and_not_the_platform_list() {
        let plan = plan_from_lock("mise.lock", LOCK, &["linux-x64".into()], &known()).unwrap();
        assert!(plan.entries.iter().all(|e| e.platform == "linux-x64"));
        assert_eq!(plan.platforms, vec!["linux-x64"]);
    }

    #[test]
    fn an_empty_registry_list_reports_every_host_as_unmirrored() {
        let plan = plan_from_lock("mise.lock", LOCK, &[], &[]).unwrap();
        assert_eq!(
            plan.unmirrored_hosts,
            vec!["binaries.sonarsource.com", "github.com"]
        );
        assert!(plan.entries.iter().all(|e| e.key.is_none()));
    }

    /// The shape `mise lock` actually writes: the platform is a *quoted key*
    /// on the tool table, not a `platforms` sub-table. Read naively it names
    /// the platform `platforms.linux-x64`, and `--platform linux-x64` then
    /// plans nothing for a lock full of tools.
    #[test]
    fn the_quoted_platform_key_mise_really_writes_is_read_as_a_platform() {
        const REAL: &str = r#"
[[tools."aqua:EmbarkStudios/cargo-deny"]]
version = "0.20.2"
backend = "aqua:EmbarkStudios/cargo-deny"

[tools."aqua:EmbarkStudios/cargo-deny"."platforms.linux-x64"]
checksum = "sha256:9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f"
url = "https://github.com/EmbarkStudios/cargo-deny/releases/download/0.20.2/cargo-deny-0.20.2-x86_64-unknown-linux-musl.tar.gz"
url_api = "https://api.github.com/repos/EmbarkStudios/cargo-deny/releases/assets/471598214"
"#;
        let plan = plan_from_lock("mise.lock", REAL, &["linux-x64".into()], &known()).unwrap();
        assert_eq!(plan.platforms, vec!["linux-x64"]);
        assert_eq!(plan.entries.len(), 2, "the download and the API address");
        assert!(plan.entries.iter().all(|e| e.platform == "linux-x64"));
    }

    /// `url_api` is the same bytes at the address the `aqua:`/`github:`
    /// backends actually fetch. A bundle carrying only the browser download
    /// URL leaves the disconnected install one `503` short.
    #[test]
    fn both_addresses_of_one_asset_are_planned_and_share_a_digest() {
        const REAL: &str = r#"
[[tools.claude]]
version = "2.1.258"
[tools.claude."platforms.linux-x64"]
checksum = "sha256:4dcbd239217dd01a5cf73002eea8f85e945170830d66891948426a09c99f3292"
url = "https://github.com/anthropics/claude-code/releases/download/v2.1.258/claude-linux-x64.tar.gz"
url_api = "https://api.github.com/repos/anthropics/claude-code/releases/assets/9911"
"#;
        let plan = plan_from_lock("mise.lock", REAL, &[], &known()).unwrap();
        let by_address: Vec<PlanAddress> = plan.entries.iter().map(|e| e.address).collect();
        assert!(by_address.contains(&PlanAddress::Download));
        assert!(by_address.contains(&PlanAddress::Api));
        // One artifact: the lock's digest is on both, so the bundle carries
        // one blob under two keys.
        let digests: BTreeSet<&str> = plan
            .entries
            .iter()
            .filter_map(|e| e.sha256.as_deref())
            .collect();
        assert_eq!(digests.len(), 1);
        let api = plan
            .entries
            .iter()
            .find(|e| e.address == PlanAddress::Api)
            .unwrap();
        assert_eq!(
            api.proxy_path.as_deref(),
            Some("/proxy/gh/anthropics/claude-code/releases/assets/9911")
        );
    }

    #[test]
    fn a_platform_is_matched_across_the_two_spellings_of_macos() {
        assert!(platform_matches("darwin-arm64", "macos-arm64"));
        assert!(platform_matches("macos-arm64", "darwin-arm64"));
        assert!(platform_matches("linux-x64", "linux-x64"));
        assert!(!platform_matches("linux-x64", "linux-arm64"));
    }

    /// RFC 0008 §13 decision 2: a bundle always carries the binary that will
    /// read the next plan.
    #[test]
    fn mise_can_carry_itself_across_the_gap() {
        let mut plan = plan_from_lock("mise.lock", LOCK, &["linux-x64".into()], &known()).unwrap();
        add_mise_itself(&mut plan, "2026.8.6", &known());
        let mise = plan
            .entries
            .iter()
            .find(|e| e.tool == "github:jdx/mise")
            .expect("the plan carries mise");
        assert_eq!(mise.version, "2026.8.6");
        assert_eq!(mise.platform, "linux-x64", "one per planned platform");
        assert_eq!(
            mise.url,
            "https://github.com/jdx/mise/releases/download/v2026.8.6/mise-v2026.8.6-linux-x64.tar.gz"
        );
        assert!(mise
            .proxy_path
            .as_deref()
            .unwrap()
            .starts_with("/proxy/gh/"));
        // Twice is once: a plan re-run must not grow.
        let before = plan.entries.len();
        add_mise_itself(&mut plan, "2026.8.6", &known());
        assert_eq!(plan.entries.len(), before);
    }

    #[test]
    fn without_a_github_registry_carrying_mise_names_the_host_instead() {
        let mut plan = plan_from_lock("mise.lock", LOCK, &["linux-x64".into()], &[]).unwrap();
        add_mise_itself(&mut plan, "2026.8.6", &[]);
        let mise = plan
            .entries
            .iter()
            .find(|e| e.tool == "github:jdx/mise")
            .unwrap();
        assert!(mise.key.is_none() && mise.proxy_path.is_none());
        assert!(plan.unmirrored_hosts.iter().any(|h| h == "github.com"));
    }

    #[test]
    fn a_lock_that_is_not_toml_is_an_error_rather_than_an_empty_plan() {
        assert!(plan_from_lock("mise.lock", "not = = toml", &[], &[]).is_err());
        // An empty but valid lock is a plan with nothing in it, which is a
        // different thing and not an error.
        let plan = plan_from_lock("mise.lock", "", &[], &[]).unwrap();
        assert!(plan.entries.is_empty());
    }
}
