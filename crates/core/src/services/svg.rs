//! An allow-list over SVG, for every SVG this server hands a browser.
//!
//! RFC 0007-bis was drafted refusing to serve SVG at all, on reasoning that is
//! correct as far as it goes: an SVG is a *document*, it can carry script, and
//! this serves it from the console's own origin. What the draft did not know is
//! that **67 % of the images in real READMEs are SVG** (RFC 0007-bis §13.2) —
//! every shields.io badge, every `opencollective` banner. Refusing them would
//! make `remote_images = "proxy"` render a third of a badge row and look broken,
//! which is RFC 0007 §4.1's `"allow"` trap wearing different clothes.
//!
//! So they are served, behind **two independent controls, either sufficient on
//! its own** (RFC 0007-bis §7.2):
//!
//! - the response carries [`SVG_SANDBOX_CSP`], which stops script in every mode
//!   a browser has, including the top-level navigation an `<img>` never performs
//!   but a reader opening the image in a new tab does. That control does not
//!   depend on this file being right.
//! - this module, which does not depend on the browser being right.
//!
//! Belt and braces here and nowhere else in the RFC, for the reason RFC 0007
//! §7.1 gives about the HTML sanitiser: this is markup an arbitrary publisher
//! authored, rendered on an origin an administrator is authenticated to.
//!
//! ## Why this is a service and not `readme::svg`
//!
//! It was `readme::svg` while the README image proxy was its only caller, and
//! RFC 0007-bis §11 q1 asked whether to move it and answered *not until there
//! are two* — generalising a security boundary that has never served two masters
//! is how it acquires an option nobody needs.
//!
//! There are two. A VSIX ships an `icon` its manifest names, the marketplace
//! asset endpoint serves it out of the archive, and it was going out as
//! `application/octet-stream` — an undownloadable non-icon — for want of exactly
//! this file (`handlers/proxy/vsx/assets.rs`). The trade that comment recorded
//! ("the editor renders no icon, which is the correct trade") was only correct
//! while the alternative was serving the bytes unexamined.
//!
//! Two callers, and both of them serve an SVG the browser is *meant to draw*.
//! That is the boundary, not "the bytes came out of an archive": the same
//! handler module serves arbitrary files inside an extension, and those keep
//! going out verbatim under an allow-list with no SVG in it, because a file
//! server that returns something other than what the publisher shipped is worse
//! than an icon that does not render.
//!
//! The move is to a *sibling of* the README service rather than inside it, so
//! neither caller reaches through the other: an extension icon has nothing to do
//! with a README, and a `use crate::services::readme::svg` in the marketplace
//! handler would have said it did. The CSP moved with it for the same reason —
//! the two controls are one decision, and a caller that took the sanitiser and
//! left the header behind would have kept the half that can be wrong.
//!
//! ## Why a re-serialising allow-list rather than a filter
//!
//! Nothing of the input survives except what an allow-list named: the document
//! is parsed with `quick-xml`, and a new one is written from the elements and
//! attributes that are permitted. A filter that deleted the bad parts would have
//! to be right about every encoding of every bad part; this has to be right about
//! the good ones, which is a much shorter list and one that fails safe.
//!
//! `quick-xml` never resolves external entities — it hands unknown ones back
//! untouched — so a hostile document gets no XXE primitive here, which is the
//! same property `crates/core`'s Maven metadata rewriting relies on.

use std::io::Cursor;

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, Writer};

/// The one spelling of the type an SVG goes out under.
///
/// Shared with the sanitiser rather than written at each call site, because the
/// two decisions are the same decision: these bytes are declared a renderable
/// document, so they went through [`sanitize_svg`] first.
pub const SVG_CONTENT_TYPE: &str = "image/svg+xml";

/// The `Content-Security-Policy` every response carrying an SVG must send.
///
/// §7.2's first control, and the one that does not depend on this module being
/// right: `sandbox` stops script in **every** mode a browser has, including the
/// top-level navigation a reader performs by opening the image in a new tab —
/// the one mode in which an SVG served from this origin would otherwise execute
/// with it.
///
/// Callers apply it to whatever they are serving and not only to SVG. A PNG has
/// nothing to lose by it, and a type-sniffing bug is exactly the case where a
/// policy conditioned on the type would be absent when it mattered.
///
/// `style-src 'unsafe-inline'` is the one relaxation: the allow-list keeps
/// presentation attributes, and a badge that lost its fills would be sanitised
/// into a blank rectangle. A style cannot read `localStorage`.
pub const SVG_SANDBOX_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; sandbox";

/// Every element an SVG we serve may contain.
///
/// Shapes, paths, text, gradients, and the grouping and clipping they need.
/// Everything else goes, and the ones worth naming because they are the attack:
/// `script` obviously; `foreignObject`, which embeds arbitrary HTML — including
/// a `<script>` — inside an SVG and is the classic way past a naive filter;
/// `use` and `image`, which load an external document; `animate` and its family,
/// which carry `onbegin`; `style`, which can `@import`; and `a`, because a badge
/// does not need to navigate and a link inside an image is a click a reader
/// cannot inspect.
const ALLOWED_ELEMENTS: &[&str] = &[
    "svg",
    "g",
    "defs",
    "title",
    "desc",
    "symbol",
    "path",
    "rect",
    "circle",
    "ellipse",
    "line",
    "polyline",
    "polygon",
    "text",
    "tspan",
    "linearGradient",
    "radialGradient",
    "stop",
    "clipPath",
    "mask",
    "pattern",
    "marker",
];

/// Every attribute those elements may carry.
///
/// Presentation and geometry only. There is no `href`, no `xlink:href`, no
/// `style`, and nothing beginning `on` — and the last is enforced by absence
/// from this list rather than by a prefix test, so an encoding trick has nothing
/// to defeat.
const ALLOWED_ATTRIBUTES: &[&str] = &[
    // Geometry
    "x",
    "y",
    "x1",
    "y1",
    "x2",
    "y2",
    "cx",
    "cy",
    "r",
    "rx",
    "ry",
    "width",
    "height",
    "d",
    "points",
    "dx",
    "dy",
    "transform",
    "viewBox",
    "preserveAspectRatio",
    "offset",
    "gradientUnits",
    "gradientTransform",
    "spreadMethod",
    "patternUnits",
    "markerWidth",
    "markerHeight",
    "refX",
    "refY",
    "orient",
    "clipPathUnits",
    "maskUnits",
    "textLength",
    "lengthAdjust",
    // Paint
    "fill",
    "fill-opacity",
    "fill-rule",
    "stroke",
    "stroke-width",
    "stroke-opacity",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-dasharray",
    "stroke-dashoffset",
    "stroke-miterlimit",
    "opacity",
    "color",
    "stop-color",
    "stop-opacity",
    "clip-path",
    "mask",
    "shape-rendering",
    // Text
    "font-family",
    "font-size",
    "font-weight",
    "font-style",
    "letter-spacing",
    "word-spacing",
    "text-anchor",
    "dominant-baseline",
    "alignment-baseline",
    // Structural. `id` stays because gradients and clip paths are referenced by
    // it *from within the same document*; the references themselves
    // (`fill="url(#g)"`) are checked below, and the document is served alone at
    // its own URL, so there is no page whose ids it could clobber.
    "id",
    "version",
    "xmlns",
    "role",
    "aria-label",
];

/// The reason a document was rejected outright rather than filtered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SvgRejected {
    /// The bytes are not XML this can parse. A truncated or malformed document
    /// is not something to guess at: a parser that recovers is a parser whose
    /// idea of the document may differ from the browser's, which is the whole
    /// mXSS family.
    ///
    /// Since quick-xml 0.42 this also covers **invalid UTF-8**: the reader
    /// validates the encoding and fails the document, where 0.41 handed the bad
    /// bytes through and this module dropped the affected text node while
    /// keeping the rest. Rejecting is the behaviour that belongs here — the
    /// alternative is emitting a document whose text this module and the
    /// browser decoded differently.
    Malformed(String),
    /// No `<svg>` root. Whatever it is, it is not the image it claimed to be.
    NotSvg,
}

impl std::fmt::Display for SvgRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(e) => write!(f, "not parseable as XML: {e}"),
            Self::NotSvg => write!(f, "no <svg> root element"),
        }
    }
}

/// Rewrite `svg` keeping only what [`ALLOWED_ELEMENTS`] and
/// [`ALLOWED_ATTRIBUTES`] name.
///
/// A disallowed element is dropped **with its subtree**, not unwrapped: unlike
/// HTML, where dropping a `<div>` and keeping its text is the useful behaviour,
/// the contents of a `<script>` or a `<foreignObject>` are the payload. Text
/// content is kept only inside `<text>`/`<tspan>`/`<title>`/`<desc>`, so a
/// badge's label survives and a stray string does not become visible markup.
///
/// Returns the rewritten document, or why it was refused entirely.
pub fn sanitize_svg(svg: &[u8]) -> Result<Vec<u8>, SvgRejected> {
    let mut reader = Reader::from_reader(svg);
    let config = reader.config_mut();
    config.trim_text(false);
    config.check_end_names = false;

    let mut walk = SvgWalk::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Err(e) => return Err(SvgRejected::Malformed(e.to_string())),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => walk.start(&e)?,
            Ok(Event::Empty(e)) => walk.empty(&e)?,
            Ok(Event::End(e)) => walk.end(&e)?,
            Ok(Event::Text(t)) => walk.text(&t)?,
            Ok(Event::GeneralRef(r)) => walk.general_ref(&r)?,
            // Everything else is dropped without comment: CDATA (a `<script>`
            // hiding place), comments (which re-parse in some engines — the mXSS
            // family), processing instructions, and the doctype, which is where
            // an entity declaration would live.
            Ok(_) => {}
        }
        buf.clear();
    }

    walk.finish()
}

/// The document being written, and what [`sanitize_svg`] must remember between
/// events to write it.
///
/// One method per event kind rather than one match arm per event kind: each arm
/// read and wrote the same three pieces of state, so none of them could be
/// understood without the others, and `skipping` in particular had four
/// different meanings depending on which arm you were standing in.
struct SvgWalk {
    writer: Writer<Cursor<Vec<u8>>>,
    /// Depth of the disallowed subtree currently being skipped. A counter rather
    /// than a flag so a `<script>` inside a `<foreignObject>` does not end the
    /// skip when the inner one closes.
    skipping: usize,
    /// Elements written, so their ends can be matched. A disallowed element's
    /// end must not be emitted even when it arrives after the skip has unwound —
    /// a malformed document can do that.
    open: Vec<String>,
    saw_root: bool,
}

impl SvgWalk {
    fn new() -> Self {
        Self {
            writer: Writer::new(Cursor::new(Vec::new())),
            skipping: 0,
            open: Vec::new(),
            saw_root: false,
        }
    }

    /// A write failure is reported as `Malformed`: the only writer here is an
    /// in-memory `Cursor`, so this is the unreachable arm rather than a mode.
    fn write(&mut self, event: Event<'_>) -> Result<(), SvgRejected> {
        self.writer
            .write_event(event)
            .map_err(|e| SvgRejected::Malformed(e.to_string()))
    }

    /// Whether text arriving now is content — inside `<text>`/`<tspan>`/
    /// `<title>`/`<desc>` and not inside a subtree being skipped.
    fn holds_text_now(&self) -> bool {
        self.skipping == 0 && self.open.last().is_some_and(|n| holds_text(n))
    }

    fn start(&mut self, element: &BytesStart<'_>) -> Result<(), SvgRejected> {
        let qname = element.name();
        let name = local_name(qname.as_ref());
        if self.skipping > 0 || !is_allowed_element(name) {
            self.skipping += 1;
            return Ok(());
        }
        if name == "svg" {
            self.saw_root = true;
        }
        let cleaned = clean_element(element, name);
        self.open.push(name.to_owned());
        self.write(Event::Start(cleaned))
    }

    fn empty(&mut self, element: &BytesStart<'_>) -> Result<(), SvgRejected> {
        let qname = element.name();
        let name = local_name(qname.as_ref());
        if self.skipping > 0 || !is_allowed_element(name) {
            return Ok(());
        }
        if name == "svg" {
            self.saw_root = true;
        }
        let cleaned = clean_element(element, name);
        self.write(Event::Empty(cleaned))
    }

    fn end(&mut self, element: &quick_xml::events::BytesEnd<'_>) -> Result<(), SvgRejected> {
        if self.skipping > 0 {
            self.skipping -= 1;
            return Ok(());
        }
        let qname = element.name();
        let name = local_name(qname.as_ref());
        if self.open.last().map(String::as_str) != Some(name) {
            return Ok(());
        }
        self.open.pop();
        self.write(Event::End(quick_xml::events::BytesEnd::new(
            name.to_owned(),
        )))
    }

    /// Text survives only where it is content rather than decoration.
    /// `quick-xml` gives it to us escaped as it appeared; writing it back
    /// through `Event::Text` re-escapes on output, so a `<` in a label cannot
    /// become a tag.
    fn text(&mut self, text: &quick_xml::events::BytesText<'_>) -> Result<(), SvgRejected> {
        if !self.holds_text_now() {
            return Ok(());
        }
        // quick-xml 0.42 stores event content as `str` and validates UTF-8 at
        // the reader, so this derefs straight to the text rather than going
        // through the old fallible `decode()`.
        let decoded = strip_forbidden_chars(text);
        self.write(Event::Text(quick_xml::events::BytesText::new(&decoded)))
    }

    /// `quick-xml` reports an entity or character reference as its own event
    /// rather than resolving it into the surrounding text, so dropping it
    /// silently would delete the `<` from a label reading `coverage &lt; 80%`.
    fn general_ref(
        &mut self,
        reference: &quick_xml::events::BytesRef<'_>,
    ) -> Result<(), SvgRejected> {
        if !self.holds_text_now() {
            return Ok(());
        }
        let Some(resolved) = resolve_reference(reference) else {
            return Ok(());
        };
        self.write(Event::Text(quick_xml::events::BytesText::new(&resolved)))
    }

    fn finish(self) -> Result<Vec<u8>, SvgRejected> {
        if !self.saw_root {
            return Err(SvgRejected::NotSvg);
        }
        Ok(self.writer.into_inner().into_inner())
    }
}

/// An attribute value as a parser will see it, so what is written is what was
/// checked.
///
/// Two normalisations, both of which a browser performs anyway:
///
/// - the characters XML 1.0 forbids are dropped ([`strip_forbidden_chars`]);
/// - tab, newline and carriage return become a space, which is what XML
///   attribute-value normalisation does on parse.
///
/// Doing it here rather than leaving it to the reader is what makes sanitising a
/// **fixed point** — sanitise twice, get the same bytes — and that property is
/// not cosmetic: it is the invariant that caught a real bypass, where a value
/// was checked in one form and emitted in another (see the call site).
fn normalize_attribute_value(raw: &str) -> String {
    strip_forbidden_chars(raw)
        .chars()
        .map(|c| {
            if matches!(c, '\t' | '\n' | '\r') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Drop the characters XML 1.0 forbids outright.
///
/// A NUL or a stray `\x01` in a text node is legal in a Rust `String` and
/// illegal in XML: a browser handed `image/svg+xml` that does not parse shows a
/// broken image, so copying one through would turn a badge into a visible defect
/// for no gain. Found by the fuzz target, which asserts the output is
/// well-formed rather than merely harmless.
///
/// Tab, newline and carriage return are the three control characters XML does
/// allow, and they are kept — a `<title>` may reasonably span lines.
fn strip_forbidden_chars(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}'))
        .collect()
}

/// The character an `&…;` reference stands for, or `None` to drop it.
///
/// The five predefined XML entities and numeric character references, and
/// **nothing else** — a name a DTD declared resolves to nothing here, which is
/// the same refusal that keeps this free of an XXE primitive. Whatever comes
/// back is written through `BytesText`, which escapes it again, so resolving
/// `&#60;` cannot put a `<` in the output.
fn resolve_reference(reference: &quick_xml::events::BytesRef<'_>) -> Option<String> {
    let name: &str = reference;
    match name {
        "lt" => return Some("<".to_owned()),
        "gt" => return Some(">".to_owned()),
        "amp" => return Some("&".to_owned()),
        "apos" => return Some("'".to_owned()),
        "quot" => return Some("\"".to_owned()),
        _ => {}
    }
    let digits = name.strip_prefix('#')?;
    let code = match digits.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse::<u32>().ok()?,
    };
    // A numeric reference can name a character XML forbids just as a literal
    // byte can — `&#1;` is the same problem spelled differently — so the same
    // rule applies rather than a second one.
    char::from_u32(code)
        .map(|c| strip_forbidden_chars(&c.to_string()))
        .filter(|s| !s.is_empty())
}

/// The element name without its namespace prefix.
///
/// `<svg:script>` is a `script`. Matching on the qualified name would let a
/// prefix declaration rename the whole attack out of the allow-list's sight.
fn local_name(qname: &str) -> &str {
    match qname.rfind(':') {
        Some(i) => &qname[i + 1..],
        None => qname,
    }
}

fn is_allowed_element(name: &str) -> bool {
    ALLOWED_ELEMENTS.contains(&name)
}

fn holds_text(name: &str) -> bool {
    matches!(name, "text" | "tspan" | "title" | "desc")
}

/// Rebuild `element` with only its allow-listed attributes.
///
/// **At most one attribute per emitted name.** The allow-list is matched on the
/// *local* name, so the namespace prefix is stripped before the attribute is
/// written back — and two source attributes can then collapse onto one output
/// name. Inkscape writes `version="1.1" inkscape:version="1.0.2"` on every file
/// it saves, which came back out as `version="1.1" version="1.0.2"`: duplicate
/// attribute names violate XML's Unique Att Spec, so a browser refuses the whole
/// document and a perfectly benign proxied logo renders as a broken image. It
/// also survived the module's idempotence test, because `with_checks(false)` is
/// exactly the setting that lets this parser read back what a browser will not.
///
/// The un-prefixed spelling wins when both are present: `version` is the SVG
/// attribute and `inkscape:version` is an editor's note that happens to share a
/// local name.
fn clean_element(element: &BytesStart<'_>, name: &str) -> BytesStart<'static> {
    // `(local name, value, was prefixed)`, in source order.
    let mut kept: Vec<(String, String, bool)> = Vec::new();
    for attribute in element.attributes().with_checks(false).flatten() {
        let qname = attribute.key.as_ref();
        let key = local_name(qname);
        if !ALLOWED_ATTRIBUTES.contains(&key) {
            continue;
        }
        // Normalise **before** deciding, not after. Found by the fuzz target,
        // as a failure of idempotence: a value reading `url\u{1}(http://evil/x)`
        // passed `safe_value` — the control character broke up the `url(` it
        // looks for — and then `strip_forbidden_chars` removed the character and
        // wrote a working external reference. The rule is to validate the bytes
        // that will be *emitted*, never the ones that arrived.
        let value = attribute
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .unwrap_or_default()
            .into_owned();
        let value = normalize_attribute_value(&value);
        if !safe_value(&value) {
            continue;
        }
        let prefixed = qname.contains(':');
        match kept.iter().position(|(seen, _, _)| seen == key) {
            Some(index) => {
                if kept[index].2 && !prefixed {
                    kept[index] = (key.to_owned(), value, prefixed);
                }
            }
            None => kept.push((key.to_owned(), value, prefixed)),
        }
    }

    let mut out = BytesStart::new(name.to_owned());
    for (key, value, _) in &kept {
        out.push_attribute((key.as_str(), value.as_str()));
    }
    out
}

/// Refuse an attribute value that reaches outside the document.
///
/// The allow-list already excludes every attribute whose *purpose* is to name a
/// URL. This covers the ones that can carry one anyway: `fill`, `stroke`,
/// `clip-path` and `mask` take a `url(…)` reference, which is legitimate when it
/// points at a `#fragment` in the same document and is a request to a
/// third-party host when it does not. A `javascript:` value is refused for the
/// same reason it is in the HTML sanitiser.
fn safe_value(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let compact: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.contains("javascript:") || compact.contains("vbscript:") || compact.contains("data:")
    {
        return false;
    }
    // `url(#local)` is fine; `url(http://…)` and `url(//…)` are not — and the
    // test is applied to **every** `url(` in the value, not just the first.
    // `fill` takes a paint server followed by a fallback, so `url(#g)
    // url(http://…)` puts a second reference behind one that passes; checking
    // only the first left the second to the browser, which is exactly the
    // beacon this module exists to refuse.
    for target in compact.split("url(").skip(1) {
        if !target.trim_start_matches(['\'', '"']).starts_with('#') {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests;
