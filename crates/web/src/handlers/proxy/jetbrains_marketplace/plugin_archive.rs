//! Plugin descriptor extraction from uploaded archives.
//!
//! A JetBrains plugin ships either as a `.jar` (with `META-INF/plugin.xml`
//! inside) or as a `.zip` distribution containing `<name>/lib/*.jar`, one of
//! which carries the descriptor. All reads are bounded (zip-bomb guard): the
//! compressed upload itself is capped by the multipart accumulation limit in
//! `publish.rs`, but a hostile archive could still declare entries that
//! decompress far larger — hence the per-jar and cumulative ceilings below.

use std::io::Read;

use quick_xml::{events::Event as XmlEvent, Reader as XmlReader};

use crate::error::AppError;

/// Decompressed ceiling for a nested `lib/*.jar` read while hunting for the
/// descriptor. Descriptor-bearing jars are typically a few MiB; the ceiling
/// stays generous for fat plugin jars without allowing quarter-GiB blowups.
const MAX_NESTED_JAR_BYTES: u64 = 128 * 1024 * 1024;
/// Cumulative decompressed budget across all nested jars of one archive, so a
/// zip with many large entries cannot force sustained per-request heap churn.
const MAX_TOTAL_NESTED_BYTES: u64 = 512 * 1024 * 1024;
/// Decompressed ceiling for `META-INF/plugin.xml` itself.
const MAX_DESCRIPTOR_BYTES: u64 = 10 * 1024 * 1024;

/// Fields parsed from `META-INF/plugin.xml`.
#[derive(Debug, Default, Clone)]
pub struct PluginDescriptor {
    pub id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub vendor: Option<String>,
    pub description: Option<String>,
    pub change_notes: Option<String>,
    pub depends: Vec<String>,
    pub since_build: Option<String>,
    pub until_build: Option<String>,
}

/// Extract and parse the plugin descriptor from an uploaded archive
/// (`.jar` direct, or `.zip` with nested `*/lib/*.jar`).
pub fn extract_plugin_descriptor(bytes: &[u8]) -> Result<PluginDescriptor, AppError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| AppError::unprocessable(format!("invalid plugin archive (not a ZIP): {e}")))?;

    // Case 1: the archive is itself the plugin jar.
    if let Some(xml) = read_descriptor_entry(&mut archive)? {
        return parse_plugin_xml(&xml);
    }

    // Case 2: zip distribution — scan nested lib/*.jar entries for the
    // descriptor. Bounded read per jar; a jar without the descriptor is skipped.
    let mut total_nested: u64 = 0;
    for i in 0..archive.len() {
        let is_lib_jar = {
            let entry = archive
                .by_index(i)
                .map_err(|e| AppError::unprocessable(format!("zip entry error: {e}")))?;
            let name = entry
                .name()
                .map_err(|e| AppError::unprocessable(format!("zip entry name: {e}")))?;
            name.contains("/lib/") && name.ends_with(".jar")
        };
        if !is_lib_jar {
            continue;
        }
        let mut entry = archive
            .by_index(i)
            .map_err(|e| AppError::unprocessable(format!("zip entry error: {e}")))?;
        let mut jar_bytes = Vec::new();
        entry
            .by_ref()
            .take(MAX_NESTED_JAR_BYTES + 1)
            .read_to_end(&mut jar_bytes)
            .map_err(|e| AppError::unprocessable(format!("reading nested jar: {e}")))?;
        if jar_bytes.len() as u64 > MAX_NESTED_JAR_BYTES {
            return Err(AppError::unprocessable(format!(
                "nested jar exceeds the {MAX_NESTED_JAR_BYTES}-byte decompressed limit"
            )));
        }
        total_nested += jar_bytes.len() as u64;
        if total_nested > MAX_TOTAL_NESTED_BYTES {
            return Err(AppError::unprocessable(format!(
                "nested jars exceed the {MAX_TOTAL_NESTED_BYTES}-byte cumulative decompressed limit"
            )));
        }
        let Ok(mut jar) = zip::ZipArchive::new(std::io::Cursor::new(jar_bytes.as_slice())) else {
            continue;
        };
        if let Some(xml) = read_descriptor_entry(&mut jar)? {
            return parse_plugin_xml(&xml);
        }
    }

    Err(AppError::unprocessable(
        "no META-INF/plugin.xml found in the plugin archive",
    ))
}

/// Read `META-INF/plugin.xml` from an open archive, bounded.
fn read_descriptor_entry<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Result<Option<Vec<u8>>, AppError> {
    let Some(index) = (0..archive.len()).find(|&i| {
        archive
            .name_for_index(i)
            .is_some_and(|n| n.is_ok_and(|n| n == "META-INF/plugin.xml"))
    }) else {
        return Ok(None);
    };
    let mut entry = archive
        .by_index(index)
        .map_err(|e| AppError::unprocessable(format!("zip entry error: {e}")))?;
    let mut buf = Vec::new();
    entry
        .by_ref()
        .take(MAX_DESCRIPTOR_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| AppError::unprocessable(format!("reading plugin.xml: {e}")))?;
    if buf.len() as u64 > MAX_DESCRIPTOR_BYTES {
        return Err(AppError::unprocessable(format!(
            "plugin.xml exceeds the {MAX_DESCRIPTOR_BYTES}-byte limit"
        )));
    }
    Ok(Some(buf))
}

/// Parse `plugin.xml` — same accumulate-text state machine as the adapter's
/// plugin-repository parser (entities arrive as separate events in quick-xml).
fn parse_plugin_xml(xml: &[u8]) -> Result<PluginDescriptor, AppError> {
    let mut reader = XmlReader::from_reader(xml);
    let mut state = PluginXmlParser::default();
    let mut buf = Vec::new();

    // One line per event kind: what each event *means* lives on `PluginXmlParser`,
    // so this loop stays a dispatch table rather than the machine itself.
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(XmlEvent::Start(e)) => state.start(&e),
            Ok(XmlEvent::Empty(e)) => state.empty(&e),
            // quick-xml 0.42 validates UTF-8 in the reader, so event content is
            // already `str` and neither of these can fail on encoding any more.
            Ok(XmlEvent::Text(e)) => state.push_text(&e),
            Ok(XmlEvent::CData(e)) => state.push_text(&e),
            Ok(XmlEvent::GeneralRef(e)) => state.push_entity(&e)?,
            Ok(XmlEvent::End(_)) => state.end(),
            Ok(XmlEvent::Eof) => break,
            Err(e) => return Err(parse_err(e)),
            _ => {}
        }
        buf.clear();
    }

    Ok(state.descriptor)
}

fn parse_err(e: impl std::fmt::Display) -> AppError {
    AppError::unprocessable(format!("plugin.xml parse: {e}"))
}

/// The bookkeeping [`parse_plugin_xml`] carries from one event to the next.
///
/// `depth == 2` is the whole selection rule: only direct children of
/// `<idea-plugin>` are descriptor fields, and a nested `<extensions>` block is
/// walked past rather than read. Text arrives in pieces — quick-xml emits
/// `Text`, `CData` and each entity reference separately — so a field's value is
/// only assigned on its `End`.
#[derive(Default)]
struct PluginXmlParser {
    descriptor: PluginDescriptor,
    current_tag: String,
    text_buf: String,
    depth: u32,
}

impl PluginXmlParser {
    /// Whether text events belong to a descriptor field right now.
    fn in_field(&self) -> bool {
        self.depth == 2 && !self.current_tag.is_empty()
    }

    fn start(&mut self, e: &quick_xml::events::BytesStart<'_>) {
        self.depth += 1;
        if self.depth != 2 {
            self.current_tag.clear();
            return;
        }
        let local = e.local_name().as_ref().to_owned();
        if local == "idea-version" {
            self.descriptor.since_build = attr(e, "since-build");
            self.descriptor.until_build = attr(e, "until-build");
        }
        self.current_tag = local;
        self.text_buf.clear();
    }

    /// `<idea-version …/>` carries its bounds on attributes, so the self-closing
    /// form is the one that actually appears.
    fn empty(&mut self, e: &quick_xml::events::BytesStart<'_>) {
        if self.depth != 1 || e.local_name().as_ref() != "idea-version" {
            return;
        }
        self.descriptor.since_build = attr(e, "since-build");
        self.descriptor.until_build = attr(e, "until-build");
    }

    fn push_text(&mut self, text: &str) {
        if self.in_field() {
            self.text_buf.push_str(text);
        }
    }

    fn push_entity(&mut self, e: &quick_xml::events::BytesRef<'_>) -> Result<(), AppError> {
        if !self.in_field() {
            return Ok(());
        }
        if let Some(ch) = e.resolve_char_ref().map_err(parse_err)? {
            self.text_buf.push(ch);
            return Ok(());
        }
        match &**e {
            "amp" => self.text_buf.push('&'),
            "lt" => self.text_buf.push('<'),
            "gt" => self.text_buf.push('>'),
            "quot" => self.text_buf.push('"'),
            "apos" => self.text_buf.push('\''),
            other => {
                self.text_buf.push('&');
                self.text_buf.push_str(other);
                self.text_buf.push(';');
            }
        }
        Ok(())
    }

    fn end(&mut self) {
        if self.depth == 2 {
            self.assign_field();
            self.current_tag.clear();
            self.text_buf.clear();
        }
        self.depth = self.depth.saturating_sub(1);
    }

    fn assign_field(&mut self) {
        let text = self.text_buf.trim();
        if text.is_empty() {
            return;
        }
        let text = text.to_owned();
        let d = &mut self.descriptor;
        match self.current_tag.as_str() {
            "id" => d.id = Some(text),
            "name" => d.name = Some(text),
            "version" => d.version = Some(text),
            "vendor" => d.vendor = Some(text),
            "description" => d.description = Some(text),
            "change-notes" => d.change_notes = Some(text),
            "depends" => d.depends.push(text),
            _ => {}
        }
    }
}

fn attr(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    e.try_get_attribute(name)
        .ok()
        .flatten()
        .and_then(|a| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok())
        .map(|v| v.into_owned())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    pub(crate) const PLUGIN_XML: &str = r#"<idea-plugin>
  <id>org.example.demo</id>
  <name>Demo &amp; More</name>
  <version>1.0.0</version>
  <vendor email="dev@example.com">Example</vendor>
  <description><![CDATA[A demo plugin]]></description>
  <change-notes>First release</change-notes>
  <depends>com.intellij.modules.platform</depends>
  <idea-version since-build="233.0" until-build="241.*"/>
  <extensions defaultExtensionNs="com.intellij">
    <id>should-not-leak-into-descriptor</id>
  </extensions>
</idea-plugin>"#;

    fn make_jar(descriptor: &str) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(&mut buf);
        w.start_file("META-INF/plugin.xml", SimpleFileOptions::default())
            .unwrap();
        w.write_all(descriptor.as_bytes()).unwrap();
        w.finish().unwrap();
        buf.into_inner()
    }

    fn make_zip_dist(descriptor: &str) -> Vec<u8> {
        let jar = make_jar(descriptor);
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(&mut buf);
        w.start_file("demo/lib/other.txt", SimpleFileOptions::default())
            .unwrap();
        w.write_all(b"not a jar").unwrap();
        w.start_file("demo/lib/demo.jar", SimpleFileOptions::default())
            .unwrap();
        w.write_all(&jar).unwrap();
        w.finish().unwrap();
        buf.into_inner()
    }

    #[test]
    fn extracts_descriptor_from_jar() {
        let d = extract_plugin_descriptor(&make_jar(PLUGIN_XML)).unwrap();
        assert_eq!(d.id.as_deref(), Some("org.example.demo"));
        assert_eq!(d.name.as_deref(), Some("Demo & More"));
        assert_eq!(d.version.as_deref(), Some("1.0.0"));
        assert_eq!(d.vendor.as_deref(), Some("Example"));
        assert_eq!(d.description.as_deref(), Some("A demo plugin"));
        assert_eq!(d.change_notes.as_deref(), Some("First release"));
        assert_eq!(d.depends, vec!["com.intellij.modules.platform"]);
        assert_eq!(d.since_build.as_deref(), Some("233.0"));
        assert_eq!(d.until_build.as_deref(), Some("241.*"));
    }

    #[test]
    fn extracts_descriptor_from_nested_zip() {
        let d = extract_plugin_descriptor(&make_zip_dist(PLUGIN_XML)).unwrap();
        assert_eq!(d.id.as_deref(), Some("org.example.demo"));
        assert_eq!(d.version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn nested_ids_do_not_overwrite_descriptor_fields() {
        let d = extract_plugin_descriptor(&make_jar(PLUGIN_XML)).unwrap();
        assert_eq!(
            d.id.as_deref(),
            Some("org.example.demo"),
            "extension-block <id> must not leak"
        );
    }

    #[test]
    fn not_a_zip_is_unprocessable() {
        assert!(extract_plugin_descriptor(b"not a zip at all").is_err());
    }

    #[test]
    fn zip_without_descriptor_is_unprocessable() {
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(&mut buf);
        w.start_file("readme.txt", SimpleFileOptions::default())
            .unwrap();
        w.write_all(b"hello").unwrap();
        w.finish().unwrap();
        assert!(extract_plugin_descriptor(&buf.into_inner()).is_err());
    }
}
