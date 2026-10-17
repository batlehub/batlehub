//! Maven: `maven-metadata.xml`.
//!
//! The only listing this proxy has to *rewrite* rather than read, and the only
//! one in XML. Two things make it more than a search-and-delete:
//!
//! - `<latest>` and `<release>` name preferred versions and come **before**
//!   `<versions>` in the document, so the rewrite is two passes: work out what
//!   survives, then stream the document out with the survivors' answers
//!   substituted.
//! - `<release>` skips qualified versions the way Maven's own resolution does —
//!   `1.0-SNAPSHOT` is a candidate for `<latest>` and never for `<release>`.
//!
//! Parsed with `quick-xml` rather than the tag-slicing the Maven adapter uses
//! for reads. A read that mis-slices returns a wrong string; a *rewrite* that
//! mis-slices emits a document Maven cannot parse at all. quick-xml also never
//! resolves external entities, so a hostile upstream gets no XXE primitive.

use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};

use super::BlockedVersions;

/// Remove blocked versions from a `maven-metadata.xml` body and repair
/// `<latest>` and `<release>`.
///
/// A document that does not parse is **left exactly as it was** and the caller
/// warned: over-listing is the safe direction, and half-rewritten XML is worse
/// than unfiltered XML.
///
/// A `<latest>` or `<release>` naming a blocked version is repaired even when
/// `<versions>` never listed it: Maven resolves `LATEST`/`RELEASE` from those
/// two elements alone, so a stale pointer kept advertising the blocked version
/// to every client that asked for one (found by `fuzz_listing_filter`).
pub fn strip_metadata(xml: &mut String, blocked: &BlockedVersions) -> Vec<String> {
    let listed = texts_of(xml, "version");
    let removed: Vec<String> = listed
        .iter()
        .filter(|v| blocked.contains(v))
        .cloned()
        .collect();
    let stale_pointer = ["latest", "release"]
        .iter()
        .any(|el| texts_of(xml, el).iter().any(|v| blocked.contains(v)));
    if removed.is_empty() && !stale_pointer {
        return removed;
    }

    let surviving: Vec<String> = listed
        .into_iter()
        .filter(|v| !blocked.contains(v))
        .collect();
    // `<latest>` is the newest *anything*, snapshots included — deliberately not
    // `best_latest`, which prefers a stable release over a newer pre-release
    // because that is what npm's `dist-tags.latest` means. Maven draws the same
    // distinction with two elements instead of one, so `<release>` is where the
    // preference for a stable version lives.
    let latest = highest(&surviving);
    let release = highest(
        &surviving
            .iter()
            .filter(|v| !is_qualified(v))
            .cloned()
            .collect::<Vec<_>>(),
    );

    // The rewrite is checked by the same reader that decided what to remove.
    // A `<version>` nested in a `<version>` is read as blocked by `texts_of`
    // but survives the streaming rewrite, which keeps only one pending element
    // — so without this check the first pass reported a removal it had not
    // made, and a second pass "removed" it again (found by
    // `fuzz_listing_filter`). Such a document is malformed metadata, and gets
    // the malformed-document answer.
    let clean = |out: &str| {
        ["version", "latest", "release"]
            .iter()
            .all(|el| texts_of(out, el).iter().all(|v| !blocked.contains(v)))
    };
    match rewrite(xml, blocked, latest.as_deref(), release.as_deref()) {
        Some(out) if clean(&out) => {
            *xml = out;
            removed
        }
        _ => {
            tracing::warn!(
                "maven-metadata.xml did not parse, or did not filter cleanly; serving it \
                 unfiltered rather than half-rewritten"
            );
            Vec::new()
        }
    }
}

/// The text of every `<{name}>` in the document, in document order.
fn texts_of(xml: &str, name: &str) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    let mut out = Vec::new();
    let mut inside = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == name => inside = true,
            Ok(Event::End(e)) if e.name().as_ref() == name => inside = false,
            Ok(Event::Text(t)) if inside => {
                let s = t.trim();
                if !s.is_empty() {
                    out.push(s.to_owned());
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// Stream the document out, dropping blocked `<version>` elements and
/// substituting the recomputed `<latest>`/`<release>` text.
///
/// `None` on a parse error, which the caller turns into "serve it unchanged".
fn rewrite(
    xml: &str,
    blocked: &BlockedVersions,
    latest: Option<&str>,
    release: Option<&str>,
) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    let mut rewriter = Rewriter::new();

    loop {
        let Ok(event) = reader.read_event() else {
            return None;
        };
        match &event {
            Event::Eof => break,
            Event::Start(e) => rewriter.start(e, latest, release)?,
            Event::End(e) => rewriter.end(e, blocked)?,
            Event::Text(t) => rewriter.text(t)?,
            other => rewriter.passthrough(other)?,
        }
    }

    rewriter.finish()
}

/// The output document being assembled by [`rewrite`], and the two pieces of
/// state that make the stream more than a copy.
///
/// Every method returns `None` on a write error, which [`rewrite`] turns into
/// "serve the document unchanged".
struct Rewriter<'a> {
    writer: Writer<Vec<u8>>,
    /// A `<version>` element is buffered because whether to emit it is only
    /// known once its text has been read.
    pending_version: Option<Vec<Event<'a>>>,
    /// The replacement text for the single-valued element currently open —
    /// `Some(None)` when nothing survives to name it with.
    substituting: Option<Option<String>>,
}

impl<'a> Rewriter<'a> {
    fn new() -> Self {
        Self {
            writer: Writer::new(Vec::new()),
            pending_version: None,
            substituting: None,
        }
    }

    fn start(
        &mut self,
        e: &BytesStart<'a>,
        latest: Option<&str>,
        release: Option<&str>,
    ) -> Option<()> {
        match e.name().as_ref() {
            "version" => self.pending_version = Some(vec![Event::Start(e.to_owned())]),
            "latest" | "release" => {
                let replacement = if e.name().as_ref() == "latest" {
                    latest
                } else {
                    release
                };
                self.substituting = Some(replacement.map(str::to_owned));
                self.writer.write_event(Event::Start(e.to_owned())).ok()?;
            }
            _ => self.passthrough(&Event::Start(e.to_owned()))?,
        }
        Some(())
    }

    fn end(&mut self, e: &BytesEnd<'a>, blocked: &BlockedVersions) -> Option<()> {
        match e.name().as_ref() {
            "version" => {
                let mut buffered = self.pending_version.take().unwrap_or_default();
                buffered.push(Event::End(e.to_owned()));
                let text = buffered
                    .iter()
                    .find_map(|ev| match ev {
                        Event::Text(t) => Some(t.trim().to_owned()),
                        _ => None,
                    })
                    .unwrap_or_default();
                if !blocked.contains(&text) {
                    for ev in buffered {
                        self.writer.write_event(ev).ok()?;
                    }
                }
                // A dropped `<version>` takes its surrounding whitespace with
                // it, because the indentation text node belonging to the *next*
                // element is written normally. Emitting it would leave a blank
                // line where the entry used to be.
            }
            "latest" | "release" => {
                self.substituting = None;
                self.writer.write_event(Event::End(e.to_owned())).ok()?;
            }
            _ => self.passthrough(&Event::End(e.to_owned()))?,
        }
        Some(())
    }

    fn text(&mut self, t: &BytesText<'a>) -> Option<()> {
        if let Some(buffered) = self.pending_version.as_mut() {
            buffered.push(Event::Text(t.to_owned()));
        } else if let Some(replacement) = self.substituting.as_ref() {
            // `None` means nothing survives to name. The element is left empty
            // rather than removed: Maven reads an absent `<release>` and an
            // empty one the same way, and removing an element mid-stream would
            // need its end tag suppressed too.
            let text = BytesText::new(replacement.as_deref().unwrap_or(""));
            self.writer.write_event(Event::Text(text)).ok()?;
        } else {
            self.writer.write_event(Event::Text(t.to_owned())).ok()?;
        }
        Some(())
    }

    /// Anything with no rewrite of its own: buffered inside a `<version>`,
    /// written straight through outside one.
    fn passthrough(&mut self, event: &Event<'a>) -> Option<()> {
        if let Some(buffered) = self.pending_version.as_mut() {
            buffered.push(event.to_owned());
        } else {
            self.writer.write_event(event.to_owned()).ok()?;
        }
        Some(())
    }

    fn finish(self) -> Option<String> {
        String::from_utf8(self.writer.into_inner()).ok()
    }
}

/// The highest of `versions`, with no preference for stable over qualified.
///
/// Semver ordering where the strings parse — which puts `2.0.0-SNAPSHOT` above
/// `1.0.0`, as Maven does — and lexicographic where they do not, since Maven
/// version strings are not required to be semver.
fn highest(versions: &[String]) -> Option<String> {
    let mut best: Option<(&String, Option<semver::Version>)> = None;
    for v in versions {
        let parsed = semver::Version::parse(v).ok();
        let wins = match (&best, &parsed) {
            (None, _) => true,
            (Some((_, Some(b))), Some(p)) => p > b,
            // A parseable version beats an unparseable one; between two
            // unparseable ones, lexicographic order is the only tiebreak there is.
            (Some((_, None)), Some(_)) => true,
            (Some((_, Some(_))), None) => false,
            (Some((b, None)), None) => v > *b,
        };
        if wins {
            best = Some((v, parsed));
        }
    }
    best.map(|(v, _)| v.clone())
}

/// Maven's `<release>` names the newest *released* version, which excludes
/// snapshots. Anything carrying a qualifier after the numeric core is a
/// candidate for `<latest>` only.
fn is_qualified(version: &str) -> bool {
    version.contains("SNAPSHOT") || version.contains('-') || version.contains("alpha")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::RegistryKind;

    fn blocked(vs: &[&str]) -> BlockedVersions {
        BlockedVersions::new(
            RegistryKind::Maven,
            vs.iter().map(|s| (*s).to_owned()).collect(),
        )
    }

    fn metadata() -> String {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<metadata>
  <groupId>com.example</groupId>
  <artifactId>mylib</artifactId>
  <versioning>
    <latest>2.0.0-SNAPSHOT</latest>
    <release>1.2.0</release>
    <versions>
      <version>1.0.0</version>
      <version>1.2.0</version>
      <version>2.0.0-SNAPSHOT</version>
    </versions>
    <lastUpdated>20240315143022</lastUpdated>
  </versioning>
</metadata>"#
            .to_owned()
    }

    #[test]
    fn a_blocked_version_leaves_the_versions_list() {
        let mut xml = metadata();
        let removed = strip_metadata(&mut xml, &blocked(&["1.2.0"]));

        assert_eq!(removed, vec!["1.2.0".to_owned()]);
        assert!(!xml.contains("<version>1.2.0</version>"));
        assert!(xml.contains("<version>1.0.0</version>"));
        assert!(xml.contains("<version>2.0.0-SNAPSHOT</version>"));
    }

    /// `<release>` must fall back to the newest surviving *non-snapshot*
    /// version, not to the newer snapshot — that is the distinction the two
    /// elements exist to draw.
    #[test]
    fn release_falls_back_past_snapshots_and_latest_does_not() {
        let mut xml = metadata();
        strip_metadata(&mut xml, &blocked(&["1.2.0"]));

        assert!(
            xml.contains("<release>1.0.0</release>"),
            "release should skip the snapshot: {xml}"
        );
        assert!(
            xml.contains("<latest>2.0.0-SNAPSHOT</latest>"),
            "latest may name a snapshot: {xml}"
        );
    }

    #[test]
    fn blocking_the_newest_release_moves_both_pointers() {
        let mut xml = metadata();
        strip_metadata(&mut xml, &blocked(&["1.2.0", "2.0.0-SNAPSHOT"]));

        assert!(xml.contains("<latest>1.0.0</latest>"), "{xml}");
        assert!(xml.contains("<release>1.0.0</release>"), "{xml}");
    }

    #[test]
    fn blocking_every_version_leaves_a_well_formed_empty_document() {
        let mut xml = metadata();
        strip_metadata(&mut xml, &blocked(&["1.0.0", "1.2.0", "2.0.0-SNAPSHOT"]));

        assert!(!xml.contains("<version>"), "{xml}");
        assert!(xml.contains("<versions>"), "the element itself survives");
        assert!(xml.contains("<latest></latest>"), "{xml}");
        assert!(
            xml.contains("<artifactId>mylib</artifactId>"),
            "the envelope survives"
        );
    }

    /// A pointer at a version `<versions>` never listed is still a pointer
    /// Maven follows for `LATEST`/`RELEASE`, so a blocked one is repaired.
    #[test]
    fn a_pointer_at_an_unlisted_blocked_version_is_repaired() {
        let mut xml =
            metadata().replace("<latest>2.0.0-SNAPSHOT</latest>", "<latest>9.9.9</latest>");
        let removed = strip_metadata(&mut xml, &blocked(&["9.9.9"]));
        assert!(removed.is_empty(), "nothing was listed to remove");
        assert!(xml.contains("<latest>2.0.0-SNAPSHOT</latest>"), "{xml}");
        assert!(xml.contains("<version>1.0.0</version>"), "{xml}");
        // And a second pass finds nothing left to do.
        let before = xml.clone();
        strip_metadata(&mut xml, &blocked(&["9.9.9"]));
        assert_eq!(xml, before);
    }

    #[test]
    fn blocking_an_absent_version_leaves_the_document_byte_identical() {
        let mut xml = metadata();
        let before = xml.clone();
        assert!(strip_metadata(&mut xml, &blocked(&["9.9.9"])).is_empty());
        assert_eq!(xml, before, "including whitespace and the XML declaration");
    }

    /// Half-rewritten XML is worse than unfiltered XML: Maven rejects the
    /// former outright and merely over-lists on the latter.
    #[test]
    fn a_malformed_document_is_returned_unchanged() {
        let mut xml = "<metadata><versions><version>1.0.0</versions>".to_owned();
        let before = xml.clone();
        let removed = strip_metadata(&mut xml, &blocked(&["1.0.0"]));

        assert!(removed.is_empty());
        assert_eq!(xml, before);
    }

    #[test]
    fn the_group_and_artifact_ids_survive_a_filter() {
        let mut xml = metadata();
        strip_metadata(&mut xml, &blocked(&["1.0.0"]));

        assert!(xml.contains("<groupId>com.example</groupId>"));
        assert!(xml.contains("<lastUpdated>20240315143022</lastUpdated>"));
    }

    #[test]
    fn snapshots_and_qualified_versions_are_not_releases() {
        assert!(is_qualified("2.0.0-SNAPSHOT"));
        assert!(is_qualified("1.0-alpha"));
        assert!(is_qualified("1.0.0-rc1"));
        assert!(!is_qualified("1.2.0"));
    }

    /// A rewrite that leaves a blocked version behind is not served: a nested
    /// `<version>` is read by one pass and kept by the other, and the
    /// document is malformed either way.
    #[test]
    fn a_nested_version_is_served_unchanged_rather_than_half_filtered() {
        let mut xml = "<version><version>x<u>2</u>1.0</version>".to_owned();
        let before = xml.clone();
        assert!(strip_metadata(&mut xml, &blocked(&["1.0"])).is_empty());
        assert_eq!(xml, before);
    }
}
