//! mise's registry archive, checked as mise checks it and merged with the
//! estate's overlay tools (RFC 0045 §4.2, §6.4).
//!
//! The reading rules mirror `parse_registry_archive` in mise v2026.11.3
//! (`src/registry.rs:382-421`): regular files at depth 2 under `registry/`,
//! ending in `.toml`, a later duplicate replacing an earlier one, and mise's
//! three limits counted as mise counts them. Every upstream entry must parse
//! and carry `version_order`, so no alias escapes collision detection and no
//! archive is served that mise would discard. The merge is deterministic: the
//! same inputs give the same bytes on every replica.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

use serde::Serialize;

/// mise's `MAX_REGISTRY_ARCHIVE_ENTRIES`.
pub const MAX_ENTRIES: usize = 4096;
/// mise's `MAX_REGISTRY_ARCHIVE_ENTRY_SIZE`.
pub const MAX_ENTRY_SIZE: u64 = 1024 * 1024;
/// mise's `MAX_REGISTRY_ARCHIVE_SIZE`, summed over entry sizes.
pub const MAX_ARCHIVE_SIZE: u64 = 16 * 1024 * 1024;
/// The compressed body this instance accepts from upstream.
pub const MAX_COMPRESSED: usize = 4 * 1024 * 1024;
/// A fixed level, so the same entries give the same bytes.
const ZSTD_LEVEL: i32 = 19;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ArchiveError {
    #[error("registry archive is over {MAX_COMPRESSED} bytes compressed")]
    TooLargeCompressed,
    #[error("registry archive contains too many entries")]
    TooManyEntries,
    #[error("registry archive entry is too large")]
    EntryTooLarge,
    #[error("registry archive is too large")]
    TooLarge,
    #[error("registry archive does not decode: {0}")]
    Decode(String),
    #[error("archive does not contain registry entries")]
    Empty,
    #[error("invalid registry/{0}.toml: {1}")]
    Entry(String, String),
}

/// The entries of a checked archive: short name → source, and each entry's
/// aliases (for collisions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryArchive {
    pub entries: BTreeMap<String, String>,
    pub aliases: BTreeMap<String, Vec<String>>,
}

/// Decode and check `raw` as mise would.
pub fn check(raw: &[u8]) -> Result<RegistryArchive, ArchiveError> {
    if raw.len() > MAX_COMPRESSED {
        return Err(ArchiveError::TooLargeCompressed);
    }
    let decoder = zstd::Decoder::new(raw).map_err(|e| ArchiveError::Decode(e.to_string()))?;
    let mut archive = tar::Archive::new(decoder);
    let mut sources: BTreeMap<String, String> = BTreeMap::new();
    let mut total = 0_u64;
    let entries = archive
        .entries()
        .map_err(|e| ArchiveError::Decode(e.to_string()))?;
    for (index, entry) in entries.enumerate() {
        let mut entry = entry.map_err(|e| ArchiveError::Decode(e.to_string()))?;
        if index >= MAX_ENTRIES {
            return Err(ArchiveError::TooManyEntries);
        }
        let size = entry.size();
        if size > MAX_ENTRY_SIZE {
            return Err(ArchiveError::EntryTooLarge);
        }
        total = total.saturating_add(size);
        if total > MAX_ARCHIVE_SIZE {
            return Err(ArchiveError::TooLarge);
        }
        if entry.header().entry_type() != tar::EntryType::Regular {
            continue;
        }
        let path = entry
            .path()
            .map_err(|e| ArchiveError::Decode(e.to_string()))?
            .into_owned();
        let components: Vec<_> = path
            .components()
            .map(|c| c.as_os_str().to_owned())
            .collect();
        if components.len() != 2 || components[0] != "registry" {
            continue;
        }
        let Some(file) = components[1].to_str() else {
            continue;
        };
        let Some(short) = file.strip_suffix(".toml") else {
            continue;
        };
        let mut source = String::new();
        // `take` keeps a lying header from reading past the checked size.
        (&mut entry)
            .take(MAX_ENTRY_SIZE)
            .read_to_string(&mut source)
            .map_err(|e| ArchiveError::Entry(short.to_owned(), e.to_string()))?;
        sources.insert(short.to_owned(), source);
    }
    if sources.is_empty() {
        return Err(ArchiveError::Empty);
    }
    let mut aliases = BTreeMap::new();
    for (short, source) in &sources {
        let value: toml::Table = toml::from_str(source)
            .map_err(|e| ArchiveError::Entry(short.clone(), e.to_string()))?;
        if !value.contains_key("version_order") {
            return Err(ArchiveError::Entry(
                short.clone(),
                "no version_order".to_owned(),
            ));
        }
        let names = match value.get("aliases") {
            None => Vec::new(),
            Some(toml::Value::Array(a)) => a
                .iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| {
                    ArchiveError::Entry(short.clone(), "aliases must be strings".to_owned())
                })?,
            Some(_) => {
                return Err(ArchiveError::Entry(
                    short.clone(),
                    "aliases must be an array".to_owned(),
                ))
            }
        };
        aliases.insert(short.clone(), names);
    }
    Ok(RegistryArchive {
        entries: sources,
        aliases,
    })
}

/// One overlay tool, its backends already written.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayEntry {
    pub name: String,
    pub aliases: Vec<String>,
    pub backends: Vec<OverlayBackend>,
    pub bins: Vec<String>,
    pub description: Option<String>,
    pub version_order: String,
    pub on_collision: Collision,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OverlayBackend {
    Plain(String),
    Full { full: String, options: toml::Table },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Collision {
    Drop,
    Overlay,
    Upstream,
}

impl Collision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Drop => "drop",
            Self::Overlay => "overlay",
            Self::Upstream => "upstream",
        }
    }
}

/// A name met on both sides, and how it was settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollisionReport {
    pub tool: String,
    pub upstream_entry: String,
    pub resolution: Collision,
}

#[derive(Serialize)]
struct EntryToml<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    aliases: &'a [String],
    backends: Vec<BackendToml<'a>>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    bins: &'a [String],
    version_order: &'a str,
}

#[derive(Serialize)]
#[serde(untagged)]
enum BackendToml<'a> {
    Plain(&'a str),
    Full {
        full: &'a str,
        #[serde(skip_serializing_if = "toml::Table::is_empty")]
        options: &'a toml::Table,
    },
}

/// The registry file of one overlay tool, serialised from a typed struct —
/// never concatenated, so no field can inject TOML (T8).
pub fn entry_toml(e: &OverlayEntry) -> Result<String, toml::ser::Error> {
    toml::to_string(&EntryToml {
        description: e.description.as_deref(),
        aliases: &e.aliases,
        backends: e
            .backends
            .iter()
            .map(|b| match b {
                OverlayBackend::Plain(s) => BackendToml::Plain(s),
                OverlayBackend::Full { full, options } => BackendToml::Full { full, options },
            })
            .collect(),
        bins: &e.bins,
        version_order: &e.version_order,
    })
}

/// `upstream` with `overlay` added, and the collisions settled (§4.2).
pub fn merge(
    upstream: &RegistryArchive,
    overlay: &[OverlayEntry],
) -> Result<(BTreeMap<String, String>, Vec<CollisionReport>), toml::ser::Error> {
    // Every name or alias of the upstream side → the entry that holds it.
    let mut upstream_names: BTreeMap<&str, &str> = BTreeMap::new();
    for (short, aliases) in &upstream.aliases {
        upstream_names.insert(short, short);
        for a in aliases {
            upstream_names.insert(a, short);
        }
    }
    let mut out = upstream.entries.clone();
    let mut dropped_upstream: BTreeSet<String> = BTreeSet::new();
    let mut reports = Vec::new();
    for tool in overlay {
        let met: BTreeSet<&str> = std::iter::once(tool.name.as_str())
            .chain(tool.aliases.iter().map(String::as_str))
            .filter_map(|n| upstream_names.get(n).copied())
            .collect();
        if met.is_empty() {
            out.insert(tool.name.clone(), entry_toml(tool)?);
            continue;
        }
        for up in &met {
            reports.push(CollisionReport {
                tool: tool.name.clone(),
                upstream_entry: (*up).to_owned(),
                resolution: tool.on_collision,
            });
        }
        match tool.on_collision {
            Collision::Drop => {
                dropped_upstream.extend(met.iter().map(|s| (*s).to_owned()));
            }
            Collision::Overlay => {
                dropped_upstream.extend(met.iter().map(|s| (*s).to_owned()));
                out.insert(tool.name.clone(), entry_toml(tool)?);
            }
            Collision::Upstream => {}
        }
    }
    for d in &dropped_upstream {
        // An overlay entry of the same short name (`"overlay"`) stays.
        if !overlay
            .iter()
            .any(|t| t.name == *d && t.on_collision == Collision::Overlay)
        {
            out.remove(d);
        }
    }
    Ok((out, reports))
}

/// Pack `entries` as `registry/<name>.toml`, in name order, with fixed
/// headers and a fixed zstd level.
pub fn pack(entries: &BTreeMap<String, String>) -> std::io::Result<Vec<u8>> {
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        for (name, source) in entries {
            let mut header = tar::Header::new_ustar();
            header.set_size(source.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_entry_type(tar::EntryType::Regular);
            builder.append_data(
                &mut header,
                format!("registry/{name}.toml"),
                source.as_bytes(),
            )?;
        }
        builder.finish()?;
    }
    zstd::encode_all(tar_bytes.as_slice(), ZSTD_LEVEL)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(files: &[(&str, &str)]) -> Vec<u8> {
        let map: BTreeMap<String, String> = files
            .iter()
            .map(|(n, s)| ((*n).to_owned(), (*s).to_owned()))
            .collect();
        pack(&map).expect("packs")
    }

    fn raw_tar(entries: &[(&str, &[u8], tar::EntryType)]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut b = tar::Builder::new(&mut buf);
            for (path, data, kind) in entries {
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_mode(0o644);
                h.set_entry_type(*kind);
                h.set_cksum();
                b.append_data(&mut h, path, *data).expect("append");
            }
            b.finish().expect("finish");
        }
        zstd::encode_all(buf.as_slice(), 3).expect("zstd")
    }

    const JQ: &str = "backends = [\"aqua:jqlang/jq\"]\nversion_order = \"source\"\n";
    const TF: &str = "backends = [\"aqua:hashicorp/terraform\"]\naliases = [\"tf\"]\nversion_order = \"semver\"\n";

    fn tool(name: &str, aliases: &[&str], c: Collision) -> OverlayEntry {
        OverlayEntry {
            name: name.to_owned(),
            aliases: aliases.iter().map(|s| (*s).to_owned()).collect(),
            backends: vec![OverlayBackend::Plain(format!("github:acme/{name}"))],
            bins: Vec::new(),
            description: None,
            version_order: "semver".to_owned(),
            on_collision: c,
        }
    }

    #[test]
    fn the_real_registry_files_check_and_round_trip() {
        let a = check(&archive(&[("jq", JQ), ("terraform", TF)])).expect("valid");
        assert_eq!(a.aliases["terraform"], vec!["tf".to_owned()]);
        let again = check(&pack(&a.entries).expect("packs")).expect("valid");
        assert_eq!(again, a);
    }

    #[test]
    fn packing_is_deterministic() {
        let files = [("jq", JQ), ("terraform", TF)];
        assert_eq!(archive(&files), archive(&files));
    }

    #[test]
    fn no_overlay_leaves_the_entries_unchanged() {
        let a = check(&archive(&[("jq", JQ)])).expect("valid");
        let (merged, reports) = merge(&a, &[]).expect("merges");
        assert_eq!(merged, a.entries);
        assert!(reports.is_empty());
    }

    #[test]
    fn overlay_cannot_replace_an_upstream_entry_without_force() {
        let a = check(&archive(&[("jq", JQ), ("terraform", TF)])).expect("valid");
        // By name, and by alias.
        for t in [
            tool("jq", &[], Collision::Drop),
            tool("other", &["tf"], Collision::Drop),
        ] {
            let (merged, reports) = merge(&a, std::slice::from_ref(&t)).expect("merges");
            assert_eq!(reports.len(), 1);
            assert!(!merged.contains_key(&t.name));
            assert!(!merged.contains_key(&reports[0].upstream_entry));
        }
        let (merged, _) = merge(&a, &[tool("jq", &[], Collision::Upstream)]).expect("merges");
        assert_eq!(merged["jq"], JQ);
        let (merged, _) = merge(&a, &[tool("jq", &[], Collision::Overlay)]).expect("merges");
        assert!(merged["jq"].contains("github:acme/jq"), "{}", merged["jq"]);
    }

    #[test]
    fn a_new_tool_is_added_and_reads_back_as_mise_reads_it() {
        let a = check(&archive(&[("jq", JQ)])).expect("valid");
        let (merged, reports) =
            merge(&a, &[tool("acme-deploy", &["deploy"], Collision::Drop)]).expect("merges");
        assert!(reports.is_empty());
        let back = check(&pack(&merged).expect("packs")).expect("mise would accept it");
        assert_eq!(back.aliases["acme-deploy"], vec!["deploy".to_owned()]);
        assert!(back.entries["acme-deploy"].contains("version_order = \"semver\""));
    }

    #[test]
    fn overlay_description_cannot_inject_toml() {
        let mut t = tool("acme", &[], Collision::Drop);
        t.description = Some("x\"\n[backends]\nevil = 1\n".to_owned());
        let s = entry_toml(&t).expect("serialises");
        let parsed: toml::Table = toml::from_str(&s).expect("parses");
        assert_eq!(parsed["description"].as_str(), t.description.as_deref());
        assert!(parsed["backends"].is_array());
    }

    #[test]
    fn archive_over_each_limit_is_refused() {
        let big = vec![b'a'; (MAX_ENTRY_SIZE + 1) as usize];
        assert_eq!(
            check(&raw_tar(&[(
                "registry/x.toml",
                &big,
                tar::EntryType::Regular
            )])),
            Err(ArchiveError::EntryTooLarge)
        );
        let many: Vec<(String, Vec<u8>)> = (0..=MAX_ENTRIES)
            .map(|i| (format!("registry/t{i}.toml"), JQ.as_bytes().to_vec()))
            .collect();
        let refs: Vec<(&str, &[u8], tar::EntryType)> = many
            .iter()
            .map(|(p, d)| (p.as_str(), d.as_slice(), tar::EntryType::Regular))
            .collect();
        assert_eq!(check(&raw_tar(&refs)), Err(ArchiveError::TooManyEntries));
        assert_eq!(
            check(&vec![0u8; MAX_COMPRESSED + 1]),
            Err(ArchiveError::TooLargeCompressed)
        );
        assert!(matches!(check(b"not zstd"), Err(ArchiveError::Decode(_))));
    }

    #[test]
    fn only_depth_two_regular_toml_files_count() {
        let a = check(&raw_tar(&[
            ("registry/jq.toml", JQ.as_bytes(), tar::EntryType::Regular),
            (
                "registry/sub/x.toml",
                JQ.as_bytes(),
                tar::EntryType::Regular,
            ),
            ("other/y.toml", JQ.as_bytes(), tar::EntryType::Regular),
            ("registry/z.txt", JQ.as_bytes(), tar::EntryType::Regular),
            ("registry/link.toml", b"", tar::EntryType::Symlink),
        ]))
        .expect("valid");
        assert_eq!(a.entries.keys().collect::<Vec<_>>(), vec!["jq"]);
    }

    #[test]
    fn an_unparseable_or_unordered_entry_fails_the_whole_check() {
        assert!(matches!(
            check(&archive(&[("jq", JQ), ("bad", "not = = toml")])),
            Err(ArchiveError::Entry(..))
        ));
        assert!(matches!(
            check(&archive(&[("jq", "backends = []\n")])),
            Err(ArchiveError::Entry(..))
        ));
        assert_eq!(check(&archive(&[])), Err(ArchiveError::Empty));
    }
}
