//! Reading a VSIX.
//!
//! A `.vsix` is a ZIP whose extension lives under `extension/`, with the
//! manifest at `extension/package.json`. Everything the gallery serves except
//! the package itself — the manifest, the README, the changelog, the licence,
//! the icon — is a file *inside* that archive, so one cached artifact answers
//! every asset request and local mode works identically to proxy mode.
//!
//! Two hazards this module exists to contain, both the same ones
//! `jetbrains_marketplace/plugin_archive.rs` guards against: a decompression
//! bomb, and a path that escapes the archive.

use std::io::{Read, Seek};

use bytes::Bytes;
use quick_xml::{events::Event as XmlEvent, Reader as XmlReader};
use serde_json::json;

use crate::error::AppError;

/// Largest single entry this will decompress. Generous for a README with
/// embedded images, far below anything that would threaten the process.
const MAX_ENTRY_BYTES: u64 = 32 * 1024 * 1024;

/// The manifest is JSON describing one extension; 10 MiB is already absurd for
/// that, and the same ceiling `plugin_archive.rs` puts on `plugin.xml`.
const MAX_MANIFEST_BYTES: u64 = 10 * 1024 * 1024;

/// Where the VS Code extension itself lives inside the archive.
const EXTENSION_PREFIX: &str = "extension/";

/// The VSIX's own manifest, beside `extension/` rather than inside it. Read for
/// exactly one thing — see [`VsixManifest::pre_release`].
const VSIX_MANIFEST_PATH: &str = "extension.vsixmanifest";

/// The `<Property>` `vsce`/`ovsx` write for a package built `--pre-release`.
const PRE_RELEASE_PROPERTY: &str = "Microsoft.VisualStudio.Code.PreRelease";

/// The fields of `extension/package.json` the gallery reports.
///
/// Deliberately a small typed subset rather than the whole manifest: these are
/// the values the two protocols actually render, and a partial parse of a
/// manifest with fields this proxy has never seen is better than refusing the
/// extension.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VsixManifest {
    pub publisher: String,
    pub name: String,
    pub version: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub categories: Vec<String>,
    pub keywords: Vec<String>,
    pub extension_pack: Vec<String>,
    pub extension_dependencies: Vec<String>,
    /// Relative path inside `extension/`, e.g. `images/icon.png`.
    pub icon: Option<String>,
    pub engines: VsixEngines,
    /// A pre-release in the VS Code sense.
    ///
    /// **Not a `package.json` field.** `vsce package --pre-release` (and
    /// `ovsx publish --pre-release`) record it as a `<Property>` in
    /// `extension.vsixmanifest`, so [`parse_manifest`] reads that file too and
    /// ORs the result in here.
    ///
    /// It has to survive a publish, because it is the one version-level marker
    /// that changes what an editor *installs*: a pre-release version is hidden
    /// from anyone who did not opt in. Dropped, a pre-release build is offered to
    /// everyone as the release.
    pub pre_release: bool,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct VsixEngines {
    pub vscode: Option<String>,
}

impl VsixManifest {
    /// `{publisher}.{name}`, the id both protocols address by.
    pub fn extension_id(&self) -> String {
        format!("{}.{}", self.publisher, self.name)
    }

    /// The publish metadata the local registry stores for this VSIX.
    ///
    /// The coordinate is passed in rather than read off the manifest, because the
    /// two publish routes disagree about who owns it: `PUT
    /// …/{extension_id}/{version}/vsix` addresses by URL and the URL wins, while
    /// `/api/-/publish` carries no coordinate at all and the manifest is all
    /// there is.
    ///
    /// Both routes build their metadata *here* rather than each assembling its
    /// own. `preRelease` decides what an editor will install (see
    /// [`Self::pre_release`]), and one route quietly omitting it publishes a
    /// pre-release as a release.
    ///
    /// Empty values are omitted; the reader
    /// (`crates/core/src/services/local_registry/eco_openvsx.rs`) treats absent
    /// as "not stated".
    pub fn index_metadata(&self, extension_id: &str, version: &str) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert("id".to_owned(), json!(extension_id));
        obj.insert("version".to_owned(), json!(version));
        obj.insert(
            "publisher".to_owned(),
            json!(extension_id.split('.').next().unwrap_or(extension_id)),
        );

        for (key, value) in [
            ("displayName", self.display_name.as_deref()),
            ("description", self.description.as_deref()),
            ("engine", self.engines.vscode.as_deref()),
            ("icon", self.icon.as_deref()),
        ] {
            if let Some(value) = value.filter(|s| !s.is_empty()) {
                obj.insert(key.to_owned(), json!(value));
            }
        }
        for (key, list) in [
            ("categories", &self.categories),
            ("keywords", &self.keywords),
            ("extensionPack", &self.extension_pack),
            ("extensionDependencies", &self.extension_dependencies),
        ] {
            if !list.is_empty() {
                obj.insert(key.to_owned(), json!(list));
            }
        }
        if self.pre_release {
            obj.insert("preRelease".to_owned(), json!(true));
        }

        serde_json::Value::Object(obj)
    }
}

/// Parse `extension/package.json` out of VSIX bytes.
///
/// `None` — never an error — when the bytes are not a ZIP, the manifest is
/// missing, or it does not parse. The raw `PUT` publish endpoint accepts
/// arbitrary bytes by design, and an extension published without a readable
/// manifest should appear in the gallery with its coordinate and nothing else
/// rather than fail to publish at all.
pub fn parse_manifest(bytes: &[u8]) -> Option<VsixManifest> {
    let cursor = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(cursor).ok()?;

    let mut manifest: VsixManifest = {
        let file = zip.by_name("extension/package.json").ok()?;
        let mut buf = Vec::new();
        file.take(MAX_MANIFEST_BYTES + 1)
            .read_to_end(&mut buf)
            .ok()?;
        if buf.len() as u64 > MAX_MANIFEST_BYTES {
            tracing::warn!("VSIX manifest exceeds {MAX_MANIFEST_BYTES} bytes; ignoring it");
            return None;
        }
        serde_json::from_slice(&buf).ok()?
    };

    // `|=` rather than `=`: a `package.json` that declared `preRelease` itself
    // is unusual but not wrong, and the two can only agree upwards.
    manifest.pre_release |= pre_release_from_vsix_manifest(&mut zip);
    Some(manifest)
}

/// Whether `extension.vsixmanifest` marks this package as a pre-release.
///
/// Best-effort in the same way as the rest of this module: a VSIX with no
/// `extension.vsixmanifest`, an unreadable one, or XML that does not parse
/// reports `false`. Publishing must not fail over a marker that most packages do
/// not carry at all.
fn pre_release_from_vsix_manifest<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> bool {
    let Ok(file) = zip.by_name(VSIX_MANIFEST_PATH) else {
        return false;
    };
    let mut xml = Vec::new();
    if file
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut xml)
        .is_err()
        || xml.len() as u64 > MAX_MANIFEST_BYTES
    {
        tracing::warn!("{VSIX_MANIFEST_PATH} is unreadable or too large; ignoring it");
        return false;
    }

    let mut reader = XmlReader::from_reader(xml.as_slice());
    let mut buf = Vec::new();
    loop {
        // `<Property …/>` is normally empty, but a `<Property></Property>` is the
        // same declaration and would otherwise be missed.
        let e = match reader.read_event_into(&mut buf) {
            Ok(XmlEvent::Empty(e)) | Ok(XmlEvent::Start(e)) => e,
            Ok(XmlEvent::Eof) => return false,
            // Malformed XML: nothing to learn from the rest of it.
            Err(e) => {
                tracing::debug!(error = %e, "{VSIX_MANIFEST_PATH} did not parse");
                return false;
            }
            _ => {
                buf.clear();
                continue;
            }
        };

        if e.local_name().as_ref() == "Property"
            && attr(&e, "Id").as_deref() == Some(PRE_RELEASE_PROPERTY)
            && attr(&e, "Value").is_some_and(|v| v.eq_ignore_ascii_case("true"))
        {
            return true;
        }
        buf.clear();
    }
}

/// One attribute of an XML element, unescaped. The same helper
/// `jetbrains_marketplace/plugin_archive.rs` uses, for the same reason.
fn attr(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    e.try_get_attribute(name)
        .ok()
        .flatten()
        .and_then(|a| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok())
        .map(|v| v.into_owned())
        .filter(|v| !v.is_empty())
}

/// Read one file out of a VSIX, by its path *relative to `extension/`*.
///
/// `Ok(None)` when the entry is absent — the caller turns that into a `404`,
/// which is what an editor expects for an extension with no changelog.
pub fn read_entry(bytes: &[u8], relative_path: &str) -> Result<Option<Bytes>, AppError> {
    // Reject traversal before it reaches the archive. `by_name` is an exact key
    // lookup rather than a filesystem join, so this is defence in depth — but a
    // `..` in the path is never legitimate and saying so at the edge keeps the
    // guarantee local to this function.
    if relative_path.is_empty()
        || relative_path
            .split('/')
            .any(|seg| seg == ".." || seg == "." || seg.is_empty())
    {
        return Err(AppError::bad_request(format!(
            "invalid path inside the extension: '{relative_path}'"
        )));
    }

    let cursor = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(cursor).map_err(|e| {
        AppError::bad_gateway(format!("extension package is not a valid VSIX: {e}"))
    })?;

    let key = format!("{EXTENSION_PREFIX}{relative_path}");
    let file = match zip.by_name(&key) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };

    let mut buf = Vec::new();
    file.take(MAX_ENTRY_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| {
            AppError::bad_gateway(format!("reading '{relative_path}' from the VSIX: {e}"))
        })?;
    if buf.len() as u64 > MAX_ENTRY_BYTES {
        return Err(AppError::bad_gateway(format!(
            "'{relative_path}' exceeds the {MAX_ENTRY_BYTES}-byte decompressed limit"
        )));
    }
    Ok(Some(Bytes::from(buf)))
}

/// The first entry under `extension/` whose name matches `predicate`, for the
/// assets whose filename is a convention rather than a manifest field —
/// `README.md`, `CHANGELOG.md`, `LICENSE.txt` and their many spellings.
pub fn find_entry(bytes: &[u8], predicate: impl Fn(&str) -> bool) -> Option<String> {
    let cursor = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(cursor).ok()?;
    let mut names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_owned()))
        .filter_map(|n| n.strip_prefix(EXTENSION_PREFIX).map(str::to_owned))
        // Top-level files only: a README nested in a subdirectory of the
        // extension is documentation *of* something inside it, not the
        // extension's own, and matching it would show the wrong document.
        .filter(|n| !n.contains('/') && !n.is_empty())
        .collect();
    // Deterministic across archives whose entry order differs.
    names.sort();
    names.into_iter().find(|n| predicate(n))
}

/// Whether this entry is an SVG, and so goes out through the sanitiser.
///
/// Split from [`content_type_for`] rather than folded into it because the answer
/// is not a type: an SVG's type depends on whether the document survived
/// [`batlehub_core::services::svg::sanitize_svg`], which this function cannot
/// know. `assets::serve_svg` asks, sanitises, and picks the type from the
/// outcome.
pub fn is_svg_path(path: &str) -> bool {
    path.rsplit('.')
        .next()
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
}

/// The `Content-Type` for a file served out of a VSIX.
///
/// These bytes are attacker-influenced and served from the same origin as the
/// admin console, which holds a bearer token — so the type is chosen from a
/// closed allowlist rather than guessed from the extension.
///
/// **SVG is deliberately absent here, and for two reasons now.** An SVG served
/// as `image/svg+xml` executes script in the document's origin, which would turn
/// "an extension shipped an icon" into console-session theft, and for as long as
/// nothing in this tree could sanitise one, the opaque download this function
/// returns was the only safe answer. The sanitiser is now a shared service
/// (RFC 0007-bis §11 q1) — but it is applied to the gallery's **icon asset**
/// alone (`assets::SvgHandling`), because the other routes `serve_entry` backs
/// serve arbitrary files inside the extension and must hand back what the
/// publisher shipped. So an SVG reaches this allow-list on every route but one,
/// and on that one only when the sanitiser *refused* it.
pub fn content_type_for(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    match lower.rsplit('.').next() {
        Some("json") => "application/json",
        Some("md") | Some("markdown") => "text/markdown; charset=utf-8",
        Some("txt") => "text/plain; charset=utf-8",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn vsix(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (name, body) in entries {
                w.start_file(*name, opts).unwrap();
                w.write_all(body).unwrap();
            }
            w.finish().unwrap();
        }
        buf
    }

    fn manifest_bytes() -> &'static [u8] {
        br#"{
            "publisher": "acme",
            "name": "tool",
            "version": "1.2.3",
            "displayName": "Acme Tool",
            "description": "Does the thing",
            "categories": ["Linters"],
            "keywords": ["acme"],
            "extensionPack": ["acme.other"],
            "extensionDependencies": ["ms-python.python"],
            "icon": "images/icon.png",
            "engines": { "vscode": "^1.85.0" }
        }"#
    }

    #[test]
    fn the_manifest_is_parsed_from_the_extension_directory() {
        let z = vsix(&[("extension/package.json", manifest_bytes())]);
        let m = parse_manifest(&z).expect("manifest parses");

        assert_eq!(m.extension_id(), "acme.tool");
        assert_eq!(m.version, "1.2.3");
        assert_eq!(m.display_name.as_deref(), Some("Acme Tool"));
        assert_eq!(m.engines.vscode.as_deref(), Some("^1.85.0"));
        assert_eq!(m.categories, ["Linters"]);
        assert_eq!(m.extension_pack, ["acme.other"]);
        assert_eq!(m.icon.as_deref(), Some("images/icon.png"));
    }

    /// `extension.vsixmanifest` as `vsce` writes it, with `properties` spliced
    /// in.
    fn vsix_manifest_xml(properties: &str) -> Vec<u8> {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
            <PackageManifest Version="2.0.0" xmlns="http://schemas.microsoft.com/developer/vsx-schema/2011">
              <Metadata>
                <Identity Language="en-US" Id="tool" Version="1.2.3" Publisher="acme" />
                <DisplayName>Acme Tool</DisplayName>
              </Metadata>
              <Installation><InstallationTarget Id="Microsoft.VisualStudio.Code" /></Installation>
              <Assets><Asset Type="Microsoft.VisualStudio.Code.Manifest" Path="extension/package.json" /></Assets>
              <Properties>
                <Property Id="Microsoft.VisualStudio.Code.Engine" Value="^1.85.0" />
                {properties}
              </Properties>
            </PackageManifest>"#
        )
        .into_bytes()
    }

    /// The marker only exists in `extension.vsixmanifest`, and it decides whether
    /// an editor offers the version to someone who did not ask for pre-releases.
    #[test]
    fn a_pre_release_package_is_recognised_from_the_vsix_manifest() {
        let xml = vsix_manifest_xml(
            r#"<Property Id="Microsoft.VisualStudio.Code.PreRelease" Value="true" />"#,
        );
        let z = vsix(&[
            ("extension/package.json", manifest_bytes()),
            (VSIX_MANIFEST_PATH, &xml),
        ]);
        assert!(parse_manifest(&z).expect("manifest parses").pre_release);
    }

    #[test]
    fn a_release_package_is_not_reported_as_a_pre_release() {
        // The property present but false, and the property absent entirely.
        for properties in [
            r#"<Property Id="Microsoft.VisualStudio.Code.PreRelease" Value="false" />"#,
            r#"<Property Id="Microsoft.VisualStudio.Code.ExecutesCode" Value="true" />"#,
        ] {
            let xml = vsix_manifest_xml(properties);
            let z = vsix(&[
                ("extension/package.json", manifest_bytes()),
                (VSIX_MANIFEST_PATH, &xml),
            ]);
            assert!(
                !parse_manifest(&z).expect("manifest parses").pre_release,
                "properties {properties}"
            );
        }

        // And a VSIX with no `extension.vsixmanifest` at all — most of what this
        // server is handed by `PUT …/vsix`.
        let z = vsix(&[("extension/package.json", manifest_bytes())]);
        assert!(!parse_manifest(&z).unwrap().pre_release);
    }

    /// A `extension.vsixmanifest` that does not parse must not fail the publish:
    /// the coordinate and everything else come from `package.json`.
    #[test]
    fn an_unparseable_vsix_manifest_degrades_to_not_pre_release() {
        let z = vsix(&[
            ("extension/package.json", manifest_bytes()),
            (VSIX_MANIFEST_PATH, b"<PackageManifest><Properties" as &[u8]),
        ]);
        let m = parse_manifest(&z).expect("the package.json still parses");
        assert!(!m.pre_release);
        assert_eq!(m.extension_id(), "acme.tool");
    }

    /// Both publish routes store metadata through this, so the keys — and
    /// `preRelease` in particular — cannot differ between them.
    #[test]
    fn the_index_metadata_carries_the_url_coordinate_and_the_pre_release_bit() {
        let z = vsix(&[("extension/package.json", manifest_bytes())]);
        let mut m = parse_manifest(&z).unwrap();

        // The `PUT` route's case: the URL disagrees with the manifest and wins.
        let meta = m.index_metadata("other.name", "9.9.9");
        assert_eq!(meta["id"], "other.name");
        assert_eq!(meta["version"], "9.9.9");
        assert_eq!(meta["publisher"], "other");
        assert_eq!(meta["displayName"], "Acme Tool");
        assert_eq!(meta["engine"], "^1.85.0");
        assert_eq!(meta["categories"], json!(["Linters"]));
        assert!(
            meta.get("preRelease").is_none(),
            "absent means 'not stated', which reads as a release"
        );

        m.pre_release = true;
        assert_eq!(m.index_metadata("acme.tool", "1.2.3")["preRelease"], true);
    }

    /// A manifest carrying fields this proxy has never modelled must still
    /// parse — refusing it would refuse the extension.
    #[test]
    fn unknown_manifest_fields_are_ignored() {
        let z = vsix(&[(
            "extension/package.json",
            br#"{"publisher":"a","name":"b","version":"1.0.0","contributes":{"commands":[]}}"#,
        )]);
        assert_eq!(parse_manifest(&z).unwrap().extension_id(), "a.b");
    }

    /// The publish endpoint accepts arbitrary bytes by design; a non-ZIP body
    /// degrades to "no manifest", not to an error.
    #[test]
    fn junk_bytes_yield_no_manifest_rather_than_an_error() {
        assert!(parse_manifest(b"PK\x03\x04not-really-a-zip").is_none());
        assert!(parse_manifest(b"").is_none());
    }

    #[test]
    fn a_vsix_without_a_manifest_yields_none() {
        let z = vsix(&[("extension/README.md", b"hi")]);
        assert!(parse_manifest(&z).is_none());
    }

    #[test]
    fn an_entry_is_read_relative_to_the_extension_directory() {
        let z = vsix(&[("extension/README.md", b"# hello")]);
        let got = read_entry(&z, "README.md").unwrap().expect("entry found");
        assert_eq!(&got[..], b"# hello");
    }

    #[test]
    fn a_missing_entry_is_none_not_an_error() {
        let z = vsix(&[("extension/package.json", manifest_bytes())]);
        assert!(read_entry(&z, "CHANGELOG.md").unwrap().is_none());
    }

    /// The path is attacker-supplied on the `unpkg` route.
    #[test]
    fn traversal_is_rejected_at_the_edge() {
        let z = vsix(&[("extension/package.json", manifest_bytes())]);
        for bad in ["../secret", "a/../../b", "./x", "", "a//b"] {
            assert!(
                read_entry(&z, bad).is_err(),
                "'{bad}' should be rejected outright"
            );
        }
    }

    #[test]
    fn find_entry_matches_top_level_files_only() {
        let z = vsix(&[
            ("extension/guide/README.md", b"not this one"),
            ("extension/README.md", b"this one"),
        ]);
        let found = find_entry(&z, |n| n.eq_ignore_ascii_case("readme.md"));
        assert_eq!(found.as_deref(), Some("README.md"));
    }

    /// An SVG never takes a type from this allow-list.
    ///
    /// On the icon asset it is routed to the sanitiser before this is consulted
    /// and only a refused document falls through; on the file routes this *is*
    /// the answer. Either way it must stay opaque.
    #[test]
    fn svg_is_not_served_as_a_renderable_image() {
        assert_eq!(content_type_for("icon.svg"), "application/octet-stream");
        assert_eq!(content_type_for("ICON.SVG"), "application/octet-stream");
    }

    #[test]
    fn known_types_are_declared_and_unknown_ones_are_opaque() {
        assert_eq!(content_type_for("package.json"), "application/json");
        assert_eq!(
            content_type_for("README.md"),
            "text/markdown; charset=utf-8"
        );
        assert_eq!(content_type_for("images/icon.png"), "image/png");
        assert_eq!(content_type_for("thing.exe"), "application/octet-stream");
        assert_eq!(content_type_for("noextension"), "application/octet-stream");
    }
}
