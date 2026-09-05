//! Listing facts read off an artifact's own bytes, once, at bundle import
//! (RFC 0008-bis §13.4).
//!
//! A synthesised listing is a projection of the held set, and for most
//! kinds the held *keys* say everything the listing needs: a version, a
//! file name, a digest. cargo's sparse index is the exception — each line
//! carries the crate's dependencies and features, and a line without them
//! is not a shorter truth but a lie cargo would build against. Those facts
//! are in the `.crate` itself (`{name}-{version}/Cargo.toml`, the
//! normalised manifest cargo publishes), so the import reads them there and
//! files them in the `meta:` entry's `extra`, where the renderer finds
//! them. An entry without them is not listed for cargo at all.

use std::io::Read;

use serde_json::{json, Map, Value};

/// The largest manifest this will read out of a crate. A real `Cargo.toml`
/// is kilobytes; the cap is against a hostile archive, not a big one.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// The facts a sparse-index line needs, from the bytes of a `.crate`:
/// `{"deps": [...], "features": {...}, "links": ...}` in the index's own
/// shapes. `None` when the archive has no readable manifest for this
/// crate, in which case the caller lists nothing rather than something
/// wrong.
pub fn cargo_index_facts(name: &str, version: &str, bytes: &[u8]) -> Option<Value> {
    let manifest = manifest_from_crate(name, version, bytes)?;
    let doc: toml::Value = toml::from_str(&manifest).ok()?;
    Some(json!({
        "deps": index_deps(&doc),
        "features": doc.get("features").map(toml_to_json).unwrap_or_else(|| json!({})),
        "links": doc
            .get("package")
            .and_then(|p| p.get("links"))
            .and_then(toml::Value::as_str)
            .map(|l| Value::String(l.to_owned()))
            .unwrap_or(Value::Null),
    }))
}

fn manifest_from_crate(name: &str, version: &str, bytes: &[u8]) -> Option<String> {
    let wanted = format!("{name}-{version}/Cargo.toml");
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz);
    for entry in archive.entries().ok()? {
        let mut entry = entry.ok()?;
        let path = entry.path().ok()?.to_string_lossy().into_owned();
        if path != wanted {
            continue;
        }
        if entry.size() > MAX_MANIFEST_BYTES {
            return None;
        }
        let mut text = String::new();
        entry.read_to_string(&mut text).ok()?;
        return Some(text);
    }
    None
}

/// `[dependencies]`, `[build-dependencies]`, `[dev-dependencies]` and every
/// `[target.'…'.<kind>]` table, as the index spells them: one object per
/// dependency with `name`, `req`, `features`, `optional`,
/// `default_features`, `target`, `kind`, and `package` when renamed.
fn index_deps(doc: &toml::Value) -> Vec<Value> {
    let mut out = Vec::new();
    for (table, kind) in [
        ("dependencies", "normal"),
        ("build-dependencies", "build"),
        ("dev-dependencies", "dev"),
    ] {
        if let Some(deps) = doc.get(table).and_then(toml::Value::as_table) {
            push_deps(&mut out, deps, kind, None);
        }
    }
    if let Some(targets) = doc.get("target").and_then(toml::Value::as_table) {
        for (target, tables) in targets {
            for (table, kind) in [
                ("dependencies", "normal"),
                ("build-dependencies", "build"),
                ("dev-dependencies", "dev"),
            ] {
                if let Some(deps) = tables.get(table).and_then(toml::Value::as_table) {
                    push_deps(&mut out, deps, kind, Some(target.as_str()));
                }
            }
        }
    }
    out
}

fn push_deps(out: &mut Vec<Value>, deps: &toml::Table, kind: &str, target: Option<&str>) {
    for (name, spec) in deps {
        let (req, features, optional, default_features, package) = match spec {
            toml::Value::String(req) => (req.clone(), Vec::new(), false, true, None),
            toml::Value::Table(t) => (
                t.get("version")
                    .and_then(toml::Value::as_str)
                    .unwrap_or("*")
                    .to_owned(),
                t.get("features")
                    .and_then(toml::Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(toml::Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
                t.get("optional")
                    .and_then(toml::Value::as_bool)
                    .unwrap_or(false),
                t.get("default-features")
                    .or_else(|| t.get("default_features"))
                    .and_then(toml::Value::as_bool)
                    .unwrap_or(true),
                t.get("package")
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned),
            ),
            _ => continue,
        };
        let mut dep = Map::new();
        // The index names the dependency by the crate it resolves to and
        // keeps the rename beside it; a manifest names it by the rename.
        let (index_name, package) = match package {
            Some(p) => (p, Some(name.clone())),
            None => (name.clone(), None),
        };
        dep.insert("name".into(), Value::String(index_name));
        dep.insert("req".into(), Value::String(req));
        dep.insert(
            "features".into(),
            Value::Array(features.into_iter().map(Value::String).collect()),
        );
        dep.insert("optional".into(), Value::Bool(optional));
        dep.insert("default_features".into(), Value::Bool(default_features));
        dep.insert(
            "target".into(),
            target
                .map(|t| Value::String(t.to_owned()))
                .unwrap_or(Value::Null),
        );
        dep.insert("kind".into(), Value::String(kind.to_owned()));
        dep.insert("registry".into(), Value::Null);
        if let Some(p) = package {
            dep.insert("package".into(), Value::String(p));
        }
        out.push(Value::Object(dep));
    }
}

fn toml_to_json(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => json!(i),
        toml::Value::Float(f) => json!(f),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(a) => Value::Array(a.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => Value::Object(
            t.iter()
                .map(|(k, v)| (k.clone(), toml_to_json(v)))
                .collect(),
        ),
    }
}

/// The largest checksum list this will read. A provider's `SHA256SUMS` is
/// one line per platform, well under a kilobyte; the cap is against a
/// hostile blob, not a big one.
const MAX_SHASUMS_BYTES: usize = 1024 * 1024;

/// The facts a provider download document needs from the checksum list a
/// Terraform provider ships beside its archives (RFC 0008-bis §13.7):
/// `{"sums": {filename: hex}}`, one entry per `<sha256>  <filename>` line.
///
/// The archive's own key names a platform (`{os}/{arch}`), not a file, and
/// Terraform looks the archive's line up in this list *by `filename`* — so
/// the name a composed document carries comes from here, matched on the
/// digest the archive's row already knows. `None` when nothing in the
/// bytes has that shape, in which case the document falls back to the
/// name HashiCorp's release tooling gives every archive.
pub fn terraform_shasums_facts(bytes: &[u8]) -> Option<Value> {
    if bytes.len() > MAX_SHASUMS_BYTES {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let mut sums = Map::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(digest), Some(file)) = (parts.next(), parts.next()) else {
            continue;
        };
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        // `sha256sum` marks a binary-mode file with a leading `*`.
        let file = file.strip_prefix('*').unwrap_or(file);
        sums.insert(file.to_owned(), Value::String(digest.to_ascii_lowercase()));
    }
    if sums.is_empty() {
        return None;
    }
    Some(json!({ "sums": sums }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn crate_with(name: &str, version: &str, manifest: &str) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            header.set_size(manifest.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(
                    &mut header,
                    format!("{name}-{version}/Cargo.toml"),
                    manifest.as_bytes(),
                )
                .unwrap();
            builder.finish().unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar_bytes).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn the_manifest_becomes_index_deps_and_features() {
        let bytes = crate_with(
            "demo",
            "1.2.3",
            r#"
[package]
name = "demo"
version = "1.2.3"
links = "demo-sys"

[dependencies]
serde = { version = "1.0", features = ["derive"], optional = true }
plain = "0.5"
renamed = { version = "2", package = "actual-crate", default-features = false }

[build-dependencies]
cc = "1"

[target.'cfg(windows)'.dependencies]
winapi = "0.3"

[features]
default = ["plain"]
extra = ["serde"]
"#,
        );
        let facts = cargo_index_facts("demo", "1.2.3", &bytes).expect("facts");
        let deps = facts["deps"].as_array().unwrap();
        assert_eq!(deps.len(), 5, "{deps:?}");
        let serde = deps.iter().find(|d| d["name"] == "serde").unwrap();
        assert_eq!(serde["req"], "1.0");
        assert_eq!(serde["features"], json!(["derive"]));
        assert_eq!(serde["optional"], true);
        assert_eq!(serde["kind"], "normal");
        let renamed = deps.iter().find(|d| d["name"] == "actual-crate").unwrap();
        assert_eq!(renamed["package"], "renamed");
        assert_eq!(renamed["default_features"], false);
        let cc = deps.iter().find(|d| d["name"] == "cc").unwrap();
        assert_eq!(cc["kind"], "build");
        let winapi = deps.iter().find(|d| d["name"] == "winapi").unwrap();
        assert_eq!(winapi["target"], "cfg(windows)");
        assert_eq!(facts["features"]["default"], json!(["plain"]));
        assert_eq!(facts["links"], "demo-sys");
    }

    #[test]
    fn a_crate_without_its_manifest_yields_nothing() {
        let bytes = crate_with("other", "0.1.0", "[package]\nname = \"other\"\n");
        assert!(cargo_index_facts("demo", "1.2.3", &bytes).is_none());
        assert!(cargo_index_facts("demo", "1.2.3", b"not a crate").is_none());
    }

    #[test]
    fn a_checksum_list_becomes_one_entry_per_archive() {
        let list = b"\
0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f9  terraform-provider-null_3.2.2_linux_amd64.zip
ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff *terraform-provider-null_3.2.2_darwin_arm64.zip
not a checksum line
";
        let facts = terraform_shasums_facts(list).expect("two lines parse");
        assert_eq!(
            facts["sums"]["terraform-provider-null_3.2.2_linux_amd64.zip"],
            "0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f9"
        );
        assert_eq!(
            facts["sums"]["terraform-provider-null_3.2.2_darwin_arm64.zip"],
            "f".repeat(64)
        );
        assert_eq!(facts["sums"].as_object().unwrap().len(), 2);
        assert!(terraform_shasums_facts(b"nothing here\n").is_none());
        assert!(terraform_shasums_facts(b"\xff\xfe").is_none());
    }
}
