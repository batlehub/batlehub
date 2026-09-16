//! Upstream response DTOs for the JetBrains Marketplace.
//!
//! The per-plugin metadata endpoint (`/plugins/list?pluginId=`) speaks the
//! classic plugin-repository XML format — one `<idea-plugin>` element per
//! published version. The search endpoint (`/api/searchPlugins`) is JSON.

use quick_xml::{events::Event as XmlEvent, Reader as XmlReader};
use serde::Deserialize;

use batlehub_core::error::CoreError;

/// One `<idea-plugin>` element from a `/plugins/list` response — a single
/// version of the plugin.
#[derive(Debug, Clone, Default)]
pub struct PluginListEntry {
    pub id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub vendor: Option<String>,
    pub description: Option<String>,
    pub change_notes: Option<String>,
    pub depends: Vec<String>,
    pub since_build: Option<String>,
    pub until_build: Option<String>,
    /// Publish date attribute, epoch milliseconds.
    pub date_ms: Option<i64>,
    pub size: Option<u64>,
}

fn attr_value(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    e.try_get_attribute(name)
        .ok()
        .flatten()
        .and_then(|a| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok())
        .map(|v| v.into_owned())
        .filter(|v| !v.is_empty())
}

fn parse_err(e: impl std::fmt::Display) -> CoreError {
    CoreError::Registry(format!("invalid plugin-repository XML: {e}"))
}

fn assign_field(entry: &mut PluginListEntry, tag: &str, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let text = text.to_owned();
    match tag {
        "id" => entry.id = Some(text),
        "name" => entry.name = Some(text),
        "version" => entry.version = Some(text),
        "vendor" => entry.vendor = Some(text),
        "description" => entry.description = Some(text),
        "change-notes" => entry.change_notes = Some(text),
        "depends" => entry.depends.push(text),
        _ => {}
    }
}

/// Parse a plugin-repository XML document into its `<idea-plugin>` entries.
///
/// Event state machine in the style of the NuGet `.nuspec` parser: elements
/// outside `<idea-plugin>` (`<plugin-repository>`, `<category>`) are skipped,
/// `<idea-version>` bounds come from attributes, `<depends>` repeats. Text is
/// accumulated across `Text`/`CData`/`GeneralRef` events — quick-xml emits
/// entity references (`&amp;`) as separate events — and assigned on the
/// element's `End`.
pub fn parse_plugin_list(xml: &[u8]) -> Result<Vec<PluginListEntry>, CoreError> {
    let mut reader = XmlReader::from_reader(xml);
    let mut state = PluginListParser::default();
    let mut buf = Vec::new();

    // One line per event kind: what each event *means* lives on `PluginListParser`,
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
            Ok(XmlEvent::End(e)) => state.end(&e),
            Ok(XmlEvent::Eof) => break,
            Err(e) => return Err(parse_err(e)),
            _ => {}
        }
        buf.clear();
    }

    Ok(state.entries)
}

/// The bookkeeping [`parse_plugin_list`] carries from one event to the next.
///
/// `current` is the `<idea-plugin>` being filled, `current_tag` the child element
/// whose text is being accumulated into `text_buf`. Text arrives in pieces —
/// quick-xml emits `Text`, `CData` and each entity reference separately — so the
/// value is only assigned on that child's `End`.
#[derive(Default)]
struct PluginListParser {
    entries: Vec<PluginListEntry>,
    current: Option<PluginListEntry>,
    current_tag: String,
    text_buf: String,
}

impl PluginListParser {
    /// Whether text events belong to a field right now, rather than to the
    /// space between elements.
    fn accumulating(&self) -> bool {
        self.current.is_some() && !self.current_tag.is_empty()
    }

    fn start(&mut self, e: &quick_xml::events::BytesStart<'_>) {
        let local = e.local_name().as_ref().to_owned();
        if local == "idea-plugin" {
            self.current = Some(PluginListEntry {
                date_ms: attr_value(e, "date").and_then(|v| v.parse().ok()),
                size: attr_value(e, "size").and_then(|v| v.parse().ok()),
                ..PluginListEntry::default()
            });
            self.current_tag.clear();
        } else if let Some(entry) = self.current.as_mut() {
            if local == "idea-version" {
                entry.since_build = attr_value(e, "since-build");
                entry.until_build = attr_value(e, "until-build");
            }
            self.current_tag = local;
            self.text_buf.clear();
        }
    }

    /// `<idea-version …/>` carries its bounds on attributes, so the self-closing
    /// form is the one that actually appears.
    fn empty(&mut self, e: &quick_xml::events::BytesStart<'_>) {
        if e.local_name().as_ref() != "idea-version" {
            return;
        }
        if let Some(entry) = self.current.as_mut() {
            entry.since_build = attr_value(e, "since-build");
            entry.until_build = attr_value(e, "until-build");
        }
    }

    fn push_text(&mut self, text: &str) {
        if self.accumulating() {
            self.text_buf.push_str(text);
        }
    }

    fn push_entity(&mut self, e: &quick_xml::events::BytesRef<'_>) -> Result<(), CoreError> {
        if !self.accumulating() {
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
            // Unknown entity: keep it verbatim rather than dropping text.
            other => {
                self.text_buf.push('&');
                self.text_buf.push_str(other);
                self.text_buf.push(';');
            }
        }
        Ok(())
    }

    fn end(&mut self, e: &quick_xml::events::BytesEnd<'_>) {
        let local = e.local_name().as_ref().to_owned();
        if local == "idea-plugin" {
            if let Some(entry) = self.current.take() {
                self.entries.push(entry);
            }
            return;
        }
        if let Some(entry) = self.current.as_mut() {
            if local == self.current_tag {
                assign_field(entry, &self.current_tag, &self.text_buf);
            }
        }
        self.current_tag.clear();
        self.text_buf.clear();
    }
}

// ── /api/searchPlugins JSON ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SearchPluginsResponse {
    #[serde(default)]
    pub plugins: Vec<SearchPluginHit>,
}

#[derive(Debug, Deserialize)]
pub struct SearchPluginHit {
    #[serde(rename = "xmlId")]
    pub xml_id: String,
    #[allow(dead_code)]
    pub name: Option<String>,
    /// Short description shown in search results.
    pub preview: Option<String>,
}

// ── /api/plugins/{id} and /api/plugins/{id}/updates JSON ─────────────────────
//
// The two documents that turn the marketplace's numeric ids into the coordinate
// this server publishes under. Both are deserialised with only the fields that
// mapping needs: these endpoints carry release notes and compatibility tables
// that would otherwise be parsed on every alias lookup for nothing.

/// One entry of `/api/plugins/{pluginId}/updates`.
#[derive(Debug, Deserialize)]
pub struct PluginUpdate {
    /// The update id — what the IDE reads out of `search/updates/compatible`
    /// and then addresses the update by.
    pub id: u64,
    /// The version string this server publishes and an operator blocks.
    pub version: Option<String>,
}

/// The identifying half of `/api/plugins/{pluginId}`.
#[derive(Debug, Deserialize)]
pub struct PluginIdentity {
    #[serde(rename = "xmlId")]
    pub xml_id: Option<String>,
}
