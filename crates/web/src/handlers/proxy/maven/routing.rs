use batlehub_core::error::CoreError;

use super::{AppError, BytesDecl, BytesEnd, BytesStart, BytesText, Event, Writer};

/// Reject a resolved Maven coordinate component that would escape the storage root
/// once interpolated into a storage key. The Maven path splitter only filters empty
/// segments, so a lone `..` can survive as the version segment — this is the edge
/// `400` that stops it before it reaches the storage backend.
fn ensure_maven_component(kind: &str, value: &str) -> Result<(), AppError> {
    batlehub_core::services::validate_path_safe(kind, value).map_err(AppError::from)
}

pub fn content_type_for(filename: &str) -> &'static str {
    if filename.ends_with(".jar") {
        "application/java-archive"
    } else if filename.ends_with(".pom") || filename.ends_with(".xml") {
        "application/xml"
    } else if filename.ends_with(".sha1")
        || filename.ends_with(".md5")
        || filename.ends_with(".sha256")
        || filename.ends_with(".sha512")
    {
        "text/plain"
    } else {
        "application/octet-stream"
    }
}

pub enum MavenPathKind {
    /// `maven-metadata.xml` request — carries the resolved `groupId:artifactId` name.
    Metadata { name: String },
    /// Normal artifact — jar, pom, checksum, etc.
    Artifact {
        name: String,
        version: String,
        filename: String,
    },
}

/// The checksum algorithms Maven asks for beside a file, by suffix.
pub const METADATA_CHECKSUMS: [&str; 4] = ["sha1", "md5", "sha256", "sha512"];

/// When the path is a checksum file *of* `maven-metadata.xml`
/// (`…/maven-metadata.xml.sha1`), the metadata's package name and the
/// algorithm. Maven fetches one beside every document; a composed
/// document has no upstream file to fetch and answers its own (RFC
/// 0008-bis §13.5).
pub fn metadata_checksum_of(maven_path: &str) -> Option<(String, &'static str)> {
    let filename = maven_path.rsplit('/').next()?;
    let algo = METADATA_CHECKSUMS
        .iter()
        .copied()
        .find(|a| filename == format!("maven-metadata.xml.{a}"))?;
    let document_path = maven_path.strip_suffix(&format!(".{algo}"))?;
    match parse_maven_path("", document_path).ok()? {
        MavenPathKind::Metadata { name } => Some((name, algo)),
        MavenPathKind::Artifact { .. } => None,
    }
}

pub fn parse_maven_path(_registry: &str, maven_path: &str) -> Result<MavenPathKind, AppError> {
    if maven_path.is_empty() {
        return Err(AppError::not_found("empty Maven path"));
    }
    let segments: Vec<&str> = maven_path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Err(AppError::not_found("invalid Maven path"));
    }

    let filename = *segments.last().expect("segments checked non-empty above");

    if filename == "maven-metadata.xml" {
        if segments.len() < 2 {
            return Err(AppError::not_found(
                "invalid Maven metadata path: missing artifactId",
            ));
        }
        let artifact_id = segments[segments.len() - 2];
        let group_segs = &segments[..segments.len() - 2];
        if group_segs.is_empty() {
            return Err(AppError::not_found(
                "invalid Maven metadata path: missing groupId",
            ));
        }
        let group_id = group_segs.join(".");
        let name = format!("{group_id}:{artifact_id}");
        ensure_maven_component("package name", &name)?;
        Ok(MavenPathKind::Metadata { name })
    } else {
        if segments.len() < 4 {
            return Err(AppError::bad_request(format!(
                "invalid Maven artifact path '{maven_path}': expected group/artifact/version/filename"
            )));
        }
        let version = segments[segments.len() - 2];
        let artifact_id = segments[segments.len() - 3];
        let group_segs = &segments[..segments.len() - 3];
        let group_id = group_segs.join(".");
        let name = format!("{group_id}:{artifact_id}");
        ensure_maven_component("package name", &name)?;
        ensure_maven_component("version", version)?;
        ensure_maven_component("filename", filename)?;
        Ok(MavenPathKind::Artifact {
            name,
            version: version.to_owned(),
            filename: filename.to_owned(),
        })
    }
}

/// Build a `maven-metadata.xml` document from locally published versions.
pub fn build_metadata_xml(
    group_id: &str,
    artifact_id: &str,
    versions: &[batlehub_core::entities::PublishedPackage],
) -> Result<String, AppError> {
    use chrono::Utc;

    let non_yanked: Vec<_> = versions.iter().filter(|v| !v.yanked).collect();

    let release = non_yanked
        .iter()
        .rfind(|v| !v.version.contains("SNAPSHOT"))
        .map(|v| v.version.as_str())
        .unwrap_or("");

    let latest = non_yanked.last().map(|v| v.version.as_str()).unwrap_or("");

    let last_updated = Utc::now().format("%Y%m%d%H%M%S").to_string();

    let mut buf = Vec::new();
    let mut w = Writer::new_with_indent(&mut buf, b' ', 2);

    w.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
        .map_err(|e| CoreError::Other(anyhow::anyhow!("xml write: {e}")))?;

    let metadata = BytesStart::new("metadata");
    w.write_event(Event::Start(metadata))
        .map_err(|e| CoreError::Other(anyhow::anyhow!("xml write: {e}")))?;

    macro_rules! leaf {
        ($w:expr, $tag:expr, $val:expr) => {{
            $w.write_event(Event::Start(BytesStart::new($tag)))
                .map_err(|e| CoreError::Other(anyhow::anyhow!("xml write: {e}")))?;
            $w.write_event(Event::Text(BytesText::new($val)))
                .map_err(|e| CoreError::Other(anyhow::anyhow!("xml write: {e}")))?;
            $w.write_event(Event::End(BytesEnd::new($tag)))
                .map_err(|e| CoreError::Other(anyhow::anyhow!("xml write: {e}")))?;
        }};
    }

    leaf!(w, "groupId", group_id);
    leaf!(w, "artifactId", artifact_id);

    w.write_event(Event::Start(BytesStart::new("versioning")))
        .map_err(|e| CoreError::Other(anyhow::anyhow!("xml write: {e}")))?;

    leaf!(w, "release", release);
    leaf!(w, "latest", latest);

    w.write_event(Event::Start(BytesStart::new("versions")))
        .map_err(|e| CoreError::Other(anyhow::anyhow!("xml write: {e}")))?;
    for v in &non_yanked {
        leaf!(w, "version", &v.version);
    }
    w.write_event(Event::End(BytesEnd::new("versions")))
        .map_err(|e| CoreError::Other(anyhow::anyhow!("xml write: {e}")))?;

    leaf!(w, "lastUpdated", &last_updated);

    w.write_event(Event::End(BytesEnd::new("versioning")))
        .map_err(|e| CoreError::Other(anyhow::anyhow!("xml write: {e}")))?;

    w.write_event(Event::End(BytesEnd::new("metadata")))
        .map_err(|e| CoreError::Other(anyhow::anyhow!("xml write: {e}")))?;

    String::from_utf8(buf).map_err(|e| CoreError::Other(anyhow::anyhow!("xml encode: {e}")).into())
}

pub struct PomMetadata {
    pub group_id: String,
    pub artifact_id: String,
    pub version: String,
    pub packaging: Option<String>,
    pub description: Option<String>,
}

pub fn parse_pom(bytes: &[u8]) -> Result<PomMetadata, AppError> {
    use quick_xml::events::Event as XE;
    use quick_xml::Reader;

    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);

    let mut group_id = None::<String>;
    let mut artifact_id = None::<String>;
    let mut version = None::<String>;
    let mut packaging = None::<String>;
    let mut description = None::<String>;
    let mut depth: u32 = 0;
    let mut current_tag = String::new();

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(XE::Start(e)) => {
                depth += 1;
                if depth == 2 {
                    current_tag = e.local_name().as_ref().to_owned();
                }
            }
            Ok(XE::Text(e)) if depth == 2 => {
                // quick-xml 0.42 validates UTF-8 in the reader, so the charset
                // decode is gone; only entity unescaping is left.
                let text = quick_xml::escape::unescape(&e)
                    .map_err(|e| AppError::unprocessable(format!("pom parse: {e}")))?
                    .into_owned();
                match current_tag.as_str() {
                    "groupId" => group_id = Some(text),
                    "artifactId" => artifact_id = Some(text),
                    "version" => version = Some(text),
                    "packaging" => packaging = Some(text),
                    "description" => description = Some(text),
                    _ => {}
                }
            }
            Ok(XE::End(_)) => {
                if depth == 2 {
                    current_tag.clear();
                }
                depth = depth.saturating_sub(1);
            }
            Ok(XE::Eof) => break,
            Err(e) => return Err(AppError::unprocessable(format!("pom parse error: {e}"))),
            _ => {}
        }
        buf.clear();
    }

    let group_id = group_id
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::unprocessable("POM missing <groupId>"))?;
    let artifact_id = artifact_id
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::unprocessable("POM missing <artifactId>"))?;
    let version = version.unwrap_or_default();

    Ok(PomMetadata {
        group_id,
        artifact_id,
        version,
        packaging,
        description,
    })
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── content_type_for ──────────────────────────────────────────────────────

    #[test]
    fn content_type_jar() {
        assert_eq!(
            content_type_for("artifact-1.0.jar"),
            "application/java-archive"
        );
    }

    #[test]
    fn content_type_pom() {
        assert_eq!(content_type_for("artifact-1.0.pom"), "application/xml");
        assert_eq!(content_type_for("maven-metadata.xml"), "application/xml");
    }

    #[test]
    fn content_type_checksums() {
        assert_eq!(content_type_for("artifact.sha1"), "text/plain");
        assert_eq!(content_type_for("artifact.md5"), "text/plain");
        assert_eq!(content_type_for("artifact.sha256"), "text/plain");
        assert_eq!(content_type_for("artifact.sha512"), "text/plain");
    }

    #[test]
    fn content_type_unknown_defaults_to_octet_stream() {
        assert_eq!(content_type_for("artifact.aar"), "application/octet-stream");
        assert_eq!(content_type_for(""), "application/octet-stream");
    }

    // ── parse_maven_path ──────────────────────────────────────────────────────

    #[test]
    fn a_metadata_checksum_path_names_the_document_and_the_algorithm() {
        let (name, algo) =
            metadata_checksum_of("org/apache/commons/commons-lang3/maven-metadata.xml.sha1")
                .unwrap();
        assert_eq!(name, "org.apache.commons:commons-lang3");
        assert_eq!(algo, "sha1");
        assert!(
            metadata_checksum_of("org/apache/commons/commons-lang3/3.12.0/x.jar.sha1").is_none()
        );
        assert!(
            metadata_checksum_of("org/apache/commons/commons-lang3/maven-metadata.xml").is_none()
        );
    }

    #[test]
    fn parse_maven_path_metadata() {
        let kind = parse_maven_path("r", "com/example/mylib/maven-metadata.xml").unwrap();
        match kind {
            MavenPathKind::Metadata { name } => assert_eq!(name, "com.example:mylib"),
            _ => panic!("expected Metadata"),
        }
    }

    #[test]
    fn parse_maven_path_artifact() {
        let kind = parse_maven_path("r", "com/example/mylib/1.0.0/mylib-1.0.0.jar").unwrap();
        match kind {
            MavenPathKind::Artifact {
                name,
                version,
                filename,
            } => {
                assert_eq!(name, "com.example:mylib");
                assert_eq!(version, "1.0.0");
                assert_eq!(filename, "mylib-1.0.0.jar");
            }
            _ => panic!("expected Artifact"),
        }
    }

    #[test]
    fn parse_maven_path_empty_returns_error() {
        assert!(parse_maven_path("r", "").is_err());
    }

    #[test]
    fn parse_maven_path_too_short_artifact_returns_error() {
        assert!(parse_maven_path("r", "mylib/1.0.0/mylib-1.0.0.jar").is_err());
    }

    #[test]
    fn parse_maven_path_metadata_missing_group_returns_error() {
        assert!(parse_maven_path("r", "maven-metadata.xml").is_err());
    }

    #[test]
    fn parse_maven_path_traversal_version_rejected() {
        // A lone `..` survives the empty-segment filter as the version segment;
        // the edge guard must reject it before it reaches the storage key.
        match parse_maven_path("r", "com/example/mylib/../mylib-1.0.0.jar") {
            Err(e) => assert_eq!(e.status, actix_web::http::StatusCode::BAD_REQUEST),
            Ok(_) => panic!("traversal version must be rejected"),
        }
    }

    // ── parse_pom ─────────────────────────────────────────────────────────────

    #[test]
    fn parse_pom_extracts_required_fields() {
        let xml = r#"<?xml version="1.0"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <groupId>com.example</groupId>
  <artifactId>mylib</artifactId>
  <version>1.2.3</version>
  <description>A test library</description>
</project>"#;
        let m = parse_pom(xml.as_bytes()).unwrap();
        assert_eq!(m.group_id, "com.example");
        assert_eq!(m.artifact_id, "mylib");
        assert_eq!(m.version, "1.2.3");
        assert_eq!(m.description.as_deref(), Some("A test library"));
    }

    #[test]
    fn parse_pom_missing_group_id_returns_error() {
        let xml = r#"<project><artifactId>mylib</artifactId></project>"#;
        assert!(parse_pom(xml.as_bytes()).is_err());
    }

    #[test]
    fn parse_pom_missing_artifact_id_returns_error() {
        let xml = r#"<project><groupId>com.example</groupId></project>"#;
        assert!(parse_pom(xml.as_bytes()).is_err());
    }

    #[test]
    fn parse_pom_missing_version_yields_empty_string() {
        let xml = r#"<project><groupId>g</groupId><artifactId>a</artifactId></project>"#;
        let m = parse_pom(xml.as_bytes()).unwrap();
        assert!(m.version.is_empty());
    }

    // ── build_metadata_xml ────────────────────────────────────────────────────

    fn make_pkg(version: &str, yanked: bool) -> batlehub_core::entities::PublishedPackage {
        use batlehub_core::entities::{PublishedPackage, Visibility};
        use chrono::Utc;
        PublishedPackage {
            registry: "maven-local".to_owned(),
            name: "com.example:mylib".to_owned(),
            version: version.to_owned(),
            checksum: "abc".to_owned(),
            yanked,
            deprecated: false,
            deprecation_message: None,
            unlisted: false,
            index_metadata: serde_json::Value::Null,
            published_at: Utc::now(),
            published_by: None,
            signature_bytes: None,
            signature_type: None,
            visibility: Visibility::default(),
            retention_keep: false,
        }
    }

    #[test]
    fn build_metadata_xml_contains_version() {
        let versions = vec![make_pkg("1.0.0", false)];
        let xml = build_metadata_xml("com.example", "mylib", &versions).unwrap();
        assert!(xml.contains("<groupId>com.example</groupId>"));
        assert!(xml.contains("<artifactId>mylib</artifactId>"));
        assert!(xml.contains("<version>1.0.0</version>"));
    }

    #[test]
    fn build_metadata_xml_excludes_yanked_versions() {
        let versions = vec![make_pkg("1.0.0", true), make_pkg("2.0.0", false)];
        let xml = build_metadata_xml("com.example", "mylib", &versions).unwrap();
        assert!(
            !xml.contains("<version>1.0.0</version>"),
            "yanked version must not appear"
        );
        assert!(xml.contains("<version>2.0.0</version>"));
    }
}
