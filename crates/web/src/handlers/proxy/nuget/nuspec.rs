use quick_xml::{events::Event as XmlEvent, Reader as XmlReader};

use crate::error::AppError;

// ── Content-type helpers ──────────────────────────────────────────────────────

pub(super) fn content_type_for(filename: &str) -> &'static str {
    if filename.ends_with(".nupkg") {
        "application/octet-stream"
    } else if filename.ends_with(".nuspec") || filename.ends_with(".xml") {
        "application/xml"
    } else if filename.ends_with(".sha512") || filename.ends_with(".sha256") {
        "text/plain"
    } else {
        "application/octet-stream"
    }
}

// ── .nuspec parser ────────────────────────────────────────────────────────────

pub(crate) struct NuspecMetadata {
    pub id: String,
    pub version: String,
    pub description: Option<String>,
    pub authors: Option<String>,
    pub tags: Option<String>,
}

struct NuspecState {
    id: Option<String>,
    version: Option<String>,
    description: Option<String>,
    authors: Option<String>,
    tags: Option<String>,
    depth: u32,
    current_tag: String,
    in_metadata: bool,
}

impl NuspecState {
    fn new() -> Self {
        Self {
            id: None,
            version: None,
            description: None,
            authors: None,
            tags: None,
            depth: 0,
            current_tag: String::new(),
            in_metadata: false,
        }
    }

    fn on_start(&mut self, local: String) {
        self.depth += 1;
        if local == "metadata" {
            self.in_metadata = true;
        }
        if self.in_metadata && self.depth == 3 {
            self.current_tag = local;
        }
    }

    fn assign_field(&mut self, text: String) {
        match self.current_tag.as_str() {
            "id" => self.id = Some(text),
            "version" => self.version = Some(text),
            "description" => self.description = Some(text),
            "authors" => self.authors = Some(text),
            "tags" => self.tags = Some(text),
            _ => {}
        }
    }

    fn on_end(&mut self) {
        if self.depth == 3 {
            self.current_tag.clear();
        }
        self.depth = self.depth.saturating_sub(1);
        if self.depth < 2 {
            self.in_metadata = false;
        }
    }

    fn into_metadata(self) -> Result<NuspecMetadata, AppError> {
        let id = self
            .id
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::unprocessable("nuspec missing <id>"))?;
        Ok(NuspecMetadata {
            id,
            version: self.version.unwrap_or_default(),
            description: self.description,
            authors: self.authors,
            tags: self.tags,
        })
    }
}

fn decode_nuspec_text(e: &quick_xml::events::BytesText) -> Result<String, AppError> {
    // quick-xml 0.42 validates UTF-8 in the reader, so the charset decode is
    // gone; only entity unescaping is left.
    Ok(quick_xml::escape::unescape(e)
        .map_err(|e| AppError::unprocessable(format!("nuspec parse: {e}")))?
        .into_owned())
}

pub(crate) fn parse_nuspec(bytes: &[u8]) -> Result<NuspecMetadata, AppError> {
    let mut reader = XmlReader::from_reader(bytes);
    reader.config_mut().trim_text(true);

    let mut state = NuspecState::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(XmlEvent::Start(e)) => {
                let local = e.local_name().as_ref().to_owned();
                state.on_start(local);
            }
            Ok(XmlEvent::Text(e)) if state.in_metadata && state.depth == 3 => {
                let text = decode_nuspec_text(&e)?;
                state.assign_field(text);
            }
            Ok(XmlEvent::End(_)) => state.on_end(),
            Ok(XmlEvent::Eof) => break,
            Err(e) => return Err(AppError::unprocessable(format!("nuspec parse error: {e}"))),
            _ => {}
        }
        buf.clear();
    }

    state.into_metadata()
}

/// Extract the `.nuspec` from a `.nupkg` ZIP archive.
pub(crate) fn extract_nuspec_from_nupkg(bytes: &[u8]) -> Result<Vec<u8>, AppError> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| AppError::unprocessable(format!("invalid .nupkg (not a ZIP): {e}")))?;

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| AppError::unprocessable(format!("zip entry error: {e}")))?;
        if file.name().ends_with(".nuspec") {
            let mut buf = Vec::new();
            use std::io::Read;
            file.read_to_end(&mut buf)
                .map_err(|e| AppError::unprocessable(format!("reading nuspec: {e}")))?;
            return Ok(buf);
        }
    }

    Err(AppError::unprocessable(
        "no .nuspec found in .nupkg archive",
    ))
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_type_nupkg() {
        assert_eq!(
            content_type_for("mylib.1.0.0.nupkg"),
            "application/octet-stream"
        );
    }

    #[test]
    fn content_type_nuspec() {
        assert_eq!(content_type_for("mylib.1.0.0.nuspec"), "application/xml");
    }

    #[test]
    fn content_type_checksum() {
        assert_eq!(content_type_for("mylib.1.0.0.sha512"), "text/plain");
        assert_eq!(content_type_for("mylib.1.0.0.sha256"), "text/plain");
    }

    #[test]
    fn content_type_unknown_defaults_to_octet_stream() {
        assert_eq!(content_type_for("file.bin"), "application/octet-stream");
        assert_eq!(content_type_for(""), "application/octet-stream");
    }

    #[test]
    fn parse_nuspec_extracts_all_fields() {
        let xml = r#"<?xml version="1.0"?>
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>MyLib</id>
    <version>1.2.3</version>
    <description>A test library</description>
    <authors>Alice, Bob</authors>
    <tags>test utils</tags>
  </metadata>
</package>"#;
        let m = parse_nuspec(xml.as_bytes()).unwrap();
        assert_eq!(m.id, "MyLib");
        assert_eq!(m.version, "1.2.3");
        assert_eq!(m.description.as_deref(), Some("A test library"));
        assert_eq!(m.authors.as_deref(), Some("Alice, Bob"));
        assert_eq!(m.tags.as_deref(), Some("test utils"));
    }

    #[test]
    fn parse_nuspec_missing_id_returns_error() {
        let xml = r#"<package><metadata><version>1.0.0</version></metadata></package>"#;
        assert!(parse_nuspec(xml.as_bytes()).is_err());
    }

    #[test]
    fn parse_nuspec_missing_version_yields_empty_string() {
        let xml = r#"<package><metadata><id>Foo</id></metadata></package>"#;
        let m = parse_nuspec(xml.as_bytes()).unwrap();
        assert_eq!(m.id, "Foo");
        assert!(m.version.is_empty());
    }

    #[test]
    fn parse_nuspec_optional_fields_absent() {
        let xml = r#"<package><metadata><id>Bare</id><version>0.1</version></metadata></package>"#;
        let m = parse_nuspec(xml.as_bytes()).unwrap();
        assert!(m.description.is_none());
        assert!(m.authors.is_none());
        assert!(m.tags.is_none());
    }
}
