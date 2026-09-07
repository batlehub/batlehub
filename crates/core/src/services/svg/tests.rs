//! The standard SVG vectors, each asserting its *specific* removal.
//!
//! A test that only greps the output for `<script` passes on a document that
//! still carries an `onload`, which is why every case below names what it
//! expects gone. The shapes are the ones that have historically defeated SVG
//! filters: namespace prefixes, `<foreignObject>`, entity-encoded handlers,
//! external `<use>`, CDATA, and `url()` values in presentation attributes.

use super::*;

fn clean(svg: &str) -> String {
    String::from_utf8(sanitize_svg(svg.as_bytes()).expect("should sanitise")).unwrap()
}

fn lower(svg: &str) -> String {
    clean(svg).to_ascii_lowercase()
}

// ── The vectors ──────────────────────────────────────────────────────────────

#[test]
fn script_goes_with_its_contents() {
    let out = lower(
        r#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script><rect width="10" height="10"/></svg>"#,
    );
    assert!(!out.contains("script"), "{out}");
    assert!(!out.contains("alert"), "{out}");
    assert!(out.contains("<rect"), "{out}");
}

/// A namespace prefix does not rename an element out of the allow-list's sight.
#[test]
fn a_namespaced_script_is_still_a_script() {
    let out = lower(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:s="http://www.w3.org/2000/svg"><s:script>alert(1)</s:script><circle r="1"/></svg>"#,
    );
    assert!(!out.contains("script"), "{out}");
    assert!(!out.contains("alert"), "{out}");
    assert!(out.contains("<circle"), "{out}");
}

/// The classic way past a naive filter: HTML, including a `<script>`, embedded
/// inside the SVG namespace.
#[test]
fn foreign_object_goes_with_everything_inside_it() {
    let out = lower(
        r#"<svg xmlns="http://www.w3.org/2000/svg"><foreignObject width="100" height="100"><body xmlns="http://www.w3.org/1999/xhtml"><script>alert(1)</script><p>text</p></body></foreignObject></svg>"#,
    );
    assert!(!out.contains("foreignobject"), "{out}");
    assert!(!out.contains("script"), "{out}");
    assert!(!out.contains("alert"), "{out}");
    // Its text is payload, not content, so it does not survive either.
    assert!(!out.contains("text"), "{out}");
}

#[test]
fn event_handlers_go_from_the_root_and_from_children() {
    let out = lower(
        r#"<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"><rect onclick="steal()" onmouseover="x" width="4" height="4"/></svg>"#,
    );
    for handler in ["onload", "onclick", "onmouseover", "alert", "steal"] {
        assert!(!out.contains(handler), "{handler} survived: {out}");
    }
    assert!(out.contains("<rect"), "{out}");
    assert!(out.contains(r#"width="4""#), "{out}");
}

/// `<animate onbegin=…>` is the handler that is not on an element anyone thinks
/// of as interactive.
#[test]
fn the_animation_family_goes_entirely() {
    for element in [
        "animate",
        "animateTransform",
        "animateMotion",
        "set",
        "handler",
    ] {
        let out = lower(&format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg"><{element} onbegin="alert(1)" attributeName="x" dur="1s"/></svg>"#
        ));
        assert!(!out.contains("onbegin"), "{element}: {out}");
        assert!(
            !out.contains(element.to_ascii_lowercase().as_str()),
            "{element}: {out}"
        );
    }
}

/// An entity-encoded handler is the same handler. It never reaches the output
/// because `on*` attributes are absent from the allow-list, so there is no
/// decoding step for an encoding to defeat.
#[test]
fn an_entity_encoded_handler_has_nothing_to_defeat() {
    let out = lower(
        r#"<svg xmlns="http://www.w3.org/2000/svg"><rect on&#108;oad="alert(1)" width="1" height="1"/></svg>"#,
    );
    assert!(!out.contains("alert"), "{out}");
    assert!(!out.contains("onload"), "{out}");
}

#[test]
fn external_references_through_use_and_image_are_dropped() {
    let out = lower(
        r##"<svg xmlns="http://www.w3.org/2000/svg"><use href="http://evil.example/x.svg#p"/><use xlink:href="//evil.example/y"/><image href="http://evil.example/z.png" width="1" height="1"/></svg>"##,
    );
    assert!(!out.contains("evil.example"), "{out}");
    assert!(!out.contains("<use"), "{out}");
    assert!(!out.contains("<image"), "{out}");
}

#[test]
fn style_and_its_import_are_dropped() {
    let out = lower(
        r#"<svg xmlns="http://www.w3.org/2000/svg"><style>@import "//evil.example/x.css";</style><rect style="background:url(javascript:alert(1))" width="1" height="1"/></svg>"#,
    );
    assert!(!out.contains("style"), "{out}");
    assert!(!out.contains("import"), "{out}");
    assert!(!out.contains("evil.example"), "{out}");
    assert!(!out.contains("javascript"), "{out}");
}

/// A `url()` in a presentation attribute is legitimate pointing at a fragment
/// in the same document — that is how every gradient is applied — and is an
/// outbound request pointing anywhere else.
#[test]
fn a_url_value_may_name_a_fragment_and_nothing_else() {
    let kept = clean(
        r##"<svg xmlns="http://www.w3.org/2000/svg"><defs><linearGradient id="g"><stop offset="0" stop-color="#fff"/></linearGradient></defs><rect fill="url(#g)" width="1" height="1"/></svg>"##,
    );
    assert!(kept.contains("url(#g)"), "{kept}");
    assert!(kept.contains("linearGradient"), "{kept}");

    for hostile in [
        r#"fill="url(http://evil.example/x)""#,
        r#"fill="url('//evil.example/x')""#,
        r#"stroke="url( http://evil.example/x )""#,
        r#"clip-path="url(https://evil.example/x)""#,
        r#"fill="javascript:alert(1)""#,
    ] {
        let out = lower(&format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg"><rect {hostile} width="1" height="1"/></svg>"#
        ));
        assert!(!out.contains("evil.example"), "{hostile} → {out}");
        assert!(!out.contains("javascript"), "{hostile} → {out}");
    }
}

/// The fragment test applies to every `url()` in the value, not to the first
/// one only.
///
/// `fill` takes a paint server with a fallback after it, so a value may hold
/// two references; checking `split_once` meant a legitimate `url(#g)` vouched
/// for whatever followed it, and the outbound request it names is the beacon
/// this module exists to refuse. Found by review, not by the corpus, because
/// every fixture had exactly one.
#[test]
fn a_second_url_value_is_checked_like_the_first() {
    for hostile in [
        r##"fill="url(#g) url(http://evil.example/x)""##,
        r##"fill="url(#g)url(//evil.example/x)""##,
        r##"mask="url(#m) url('https://evil.example/x')""##,
    ] {
        let out = lower(&format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg"><rect {hostile} width="1" height="1"/></svg>"#
        ));
        assert!(!out.contains("evil.example"), "{hostile} → {out}");
    }
}

/// Checking every `url()` must not cost the valid shape that has two tokens.
///
/// SVG's `<paint>` is `<url> [none | <color>]?` — a paint server *with a
/// fallback* is the ordinary way to write one, and the obvious way to reject a
/// trailing external reference is a rule that also rejects `url(#g) blue`. The
/// tightening above is only correct if this still renders.
#[test]
fn a_fragment_with_a_fallback_colour_is_still_allowed() {
    for valid in [
        r##"fill="url(#g) blue""##,
        r##"fill="url('#g') #fff""##,
        r##"clip-path="url(#c)""##,
    ] {
        let out = lower(&format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg"><rect {valid} width="1" height="1"/></svg>"#
        ));
        assert!(out.contains("url("), "{valid} was dropped → {out}");
    }
}

/// A link inside an image is a click a reader cannot inspect before making.
#[test]
fn an_anchor_inside_the_image_is_dropped() {
    let out = lower(
        r#"<svg xmlns="http://www.w3.org/2000/svg"><a href="http://evil.example"><rect width="1" height="1"/></a></svg>"#,
    );
    assert!(!out.contains("evil.example"), "{out}");
    assert!(!out.contains("<a "), "{out}");
}

/// CDATA is where a `<script>` hides from a filter that only looks at elements.
/// It is dropped as a node kind, so there is nothing to look inside.
#[test]
fn cdata_comments_and_the_doctype_are_dropped() {
    let out = lower(
        r#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY x "y">]><svg xmlns="http://www.w3.org/2000/svg"><![CDATA[<script>alert(1)</script>]]><!--<img src=x onerror=alert(1)>--><rect width="1" height="1"/></svg>"#,
    );
    assert!(!out.contains("script"), "{out}");
    assert!(!out.contains("alert"), "{out}");
    assert!(!out.contains("onerror"), "{out}");
    assert!(!out.contains("entity"), "{out}");
    assert!(out.contains("<rect"), "{out}");
}

/// The counter, not a flag: the inner `</script>` must not end the skip and let
/// the rest of the `<foreignObject>` back in.
#[test]
fn a_nested_disallowed_element_does_not_end_the_skip_early() {
    let out = lower(
        r#"<svg xmlns="http://www.w3.org/2000/svg"><foreignObject><script>alert(1)</script><iframe src="http://evil.example"></iframe></foreignObject><rect width="1" height="1"/></svg>"#,
    );
    assert!(!out.contains("iframe"), "{out}");
    assert!(!out.contains("evil.example"), "{out}");
    assert!(out.contains("<rect"), "{out}");
}

// ── What has to survive ──────────────────────────────────────────────────────

/// A sanitiser that empties every badge is a sanitiser nobody keeps enabled, so
/// this is as much a requirement as the removals above.
#[test]
fn a_real_badge_still_says_what_it_said() {
    let badge = r##"<svg xmlns="http://www.w3.org/2000/svg" width="104" height="20" role="img" aria-label="build: passing">
  <title>build: passing</title>
  <linearGradient id="s" x2="0" y2="100%"><stop offset="0" stop-color="#bbb" stop-opacity=".1"/><stop offset="1" stop-opacity=".1"/></linearGradient>
  <clipPath id="r"><rect width="104" height="20" rx="3" fill="#fff"/></clipPath>
  <g clip-path="url(#r)">
    <rect width="37" height="20" fill="#555"/>
    <rect x="37" width="67" height="20" fill="#4c1"/>
    <rect width="104" height="20" fill="url(#s)"/>
  </g>
  <g fill="#fff" text-anchor="middle" font-family="Verdana,sans-serif" font-size="110">
    <text x="195" y="140" transform="scale(.1)" textLength="270">build</text>
    <text x="695" y="140" transform="scale(.1)" textLength="570">passing</text>
  </g>
</svg>"##;
    let out = clean(badge);
    assert!(out.contains("build"), "{out}");
    assert!(out.contains("passing"), "{out}");
    assert!(out.contains("linearGradient"), "{out}");
    assert!(out.contains("url(#s)"), "{out}");
    assert!(out.contains("clip-path"), "{out}");
    assert!(out.contains(r#"font-family="Verdana,sans-serif""#), "{out}");
    // Both text runs, and both `<g>` wrappers.
    assert_eq!(out.matches("<text").count(), 2, "{out}");
    assert_eq!(out.matches("<g").count(), 2, "{out}");
}

/// A `<` in a label is text, and comes back as text.
#[test]
fn markup_significant_characters_in_a_label_are_re_escaped() {
    let out =
        clean(r#"<svg xmlns="http://www.w3.org/2000/svg"><text>coverage &lt; 80%</text></svg>"#);
    assert!(out.contains("&lt;"), "{out}");
    assert!(!out.contains("< 80"), "{out}");
}

/// Found by the fuzz target: a control character XML forbids, copied through
/// into a text node, makes the whole document unparseable — so a browser handed
/// `image/svg+xml` shows a broken image instead of a badge. Harmless, and a
/// visible defect, which is why the target asserts well-formedness and not only
/// safety.
#[test]
fn characters_xml_forbids_are_dropped_from_text_and_attributes() {
    let out = clean("<svg xmlns=\"http://www.w3.org/2000/svg\"><text id=\"a\u{1}b\">lab\u{0}el\u{7}</text></svg>");
    assert!(out.contains("label"), "{out}");
    assert!(out.contains(r#"id="ab""#), "{out}");
    for forbidden in ['\u{0}', '\u{1}', '\u{7}'] {
        assert!(!out.contains(forbidden), "{forbidden:?} survived: {out:?}");
    }
    // A numeric reference naming one is the same problem spelled differently.
    let out = clean("<svg xmlns=\"http://www.w3.org/2000/svg\"><title>a&#1;b</title></svg>");
    assert!(out.contains("ab"), "{out:?}");
    assert!(!out.contains('\u{1}'), "{out:?}");
    // The three XML does allow are kept.
    let out = clean("<svg xmlns=\"http://www.w3.org/2000/svg\"><title>a\tb\nc</title></svg>");
    assert!(out.contains('\t') && out.contains('\n'), "{out:?}");
}

/// The bypass the fuzz target found, as a failure of idempotence.
///
/// A control character inside `url(` split the token `safe_value` looks for, so
/// the check passed — and then the character was stripped, leaving a working
/// reference to a third-party host in the output. Sanitising twice gave a
/// different answer from sanitising once, which is exactly the shape that says a
/// decision was taken about bytes other than the ones written.
#[test]
fn a_control_character_cannot_smuggle_an_external_reference_past_the_check() {
    for hostile in [
        "url\u{1}(http://evil.example/x)",
        "url(\u{0}http://evil.example/x)",
        "u\u{7}rl(http://evil.example/x)",
    ] {
        let out = clean(&format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><rect fill=\"{hostile}\" width=\"1\" height=\"1\"/></svg>"
        ));
        assert!(!out.contains("evil.example"), "{hostile:?} → {out:?}");
    }
}

/// Sanitising the output again must produce the same bytes.
///
/// The invariant that catches a decision taken about bytes other than the ones
/// emitted — a shape that means one thing on the first pass and another on the
/// second is the mXSS family in one sentence, and the fuzz target asserts it on
/// arbitrary input.
#[test]
fn sanitising_is_idempotent_over_the_corpus() {
    for input in [
        r##"<svg xmlns="http://www.w3.org/2000/svg"><rect fill="url(#g)"/></svg>"##,
        "<svg><text>lab\u{1}el</text></svg>",
        "<svg d=\"  url \u{1}(0ml d=\"><g></g></svg>",
        r#"<svg><foreignObject><script>alert(1)</script></foreignObject></svg>"#,
    ] {
        let Ok(once) = sanitize_svg(input.as_bytes()) else {
            continue;
        };
        let twice = sanitize_svg(&once).expect("output must itself sanitise");
        assert_eq!(
            String::from_utf8_lossy(&once),
            String::from_utf8_lossy(&twice),
            "{input:?}"
        );
    }
}

// ── Refusals ─────────────────────────────────────────────────────────────────

/// A parser that recovers from malformed input is a parser whose idea of the
/// document may differ from the browser's, which is the whole mXSS family. This
/// refuses instead, and the endpoint turns the refusal into the chip.
#[test]
fn a_document_with_no_svg_root_is_refused_rather_than_emptied() {
    assert_eq!(
        sanitize_svg(b"<html><body>not an image</body></html>"),
        Err(SvgRejected::NotSvg)
    );
    assert_eq!(sanitize_svg(b""), Err(SvgRejected::NotSvg));
    assert!(matches!(
        sanitize_svg(b"<svg><rect"),
        Err(SvgRejected::Malformed(_))
    ));
}

/// Invalid UTF-8 refuses the whole document rather than being sanitised around.
///
/// This is quick-xml 0.42's behaviour and the reason to want it: 0.41 handed the
/// undecodable bytes through, and this module dropped the affected text node and
/// emitted the rest — a document whose text this module and the browser had
/// decoded differently, which is the mXSS shape the refusals above exist to
/// avoid. `0xff` is not a valid UTF-8 lead byte in any position.
#[test]
fn invalid_utf8_is_refused_rather_than_sanitised_around() {
    let mut bytes = b"<svg><title>badge".to_vec();
    bytes.push(0xff);
    bytes.extend_from_slice(b"</title></svg>");
    assert!(matches!(
        sanitize_svg(&bytes),
        Err(SvgRejected::Malformed(_))
    ));
}

/// PNG bytes are not an SVG, and the endpoint must not be able to reach this
/// with them anyway — but if it did, the answer is a refusal, not a panic.
#[test]
fn binary_input_is_refused_not_panicked_on() {
    let png = [
        0x89u8, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13,
    ];
    assert!(sanitize_svg(&png).is_err());
}

// ── The corpus the fuzz target also runs ─────────────────────────────────────

/// The same invariant `fuzz/fuzz_targets/fuzz_svg_sanitize.rs` asserts, over a
/// fixed corpus, so it is checked by `cargo test` and not only by a nightly job
/// somebody has to remember to run.
#[test]
fn nothing_in_the_corpus_yields_script_a_handler_or_an_external_reference() {
    const CORPUS: &[&str] = &[
        r#"<svg><script>alert(1)</script></svg>"#,
        r#"<svg><scr<script>ipt>alert(1)</script></svg>"#,
        r#"<svg><SCRIPT SRC="//evil/x.js"></SCRIPT></svg>"#,
        r#"<svg onload="alert(1)"></svg>"#,
        r#"<svg><animate onbegin="alert(1)" attributeName="x"/></svg>"#,
        r#"<svg><set onbegin="alert(1)"/></svg>"#,
        r#"<svg><foreignObject><script>alert(1)</script></foreignObject></svg>"#,
        r#"<svg><foreignObject><iframe src="//evil"></iframe></foreignObject></svg>"#,
        r#"<svg><use href="//evil/x.svg#a"/></svg>"#,
        r#"<svg><use xlink:href="data:image/svg+xml;base64,PHN2Zz4="/></svg>"#,
        r#"<svg><image href="//evil/x.png"/></svg>"#,
        r#"<svg><style>@import "//evil/x.css";</style></svg>"#,
        r#"<svg><rect fill="url(//evil/x)"/></svg>"#,
        r#"<svg><rect style="fill:url(javascript:alert(1))"/></svg>"#,
        r#"<svg><a href="javascript:alert(1)"><rect/></a></svg>"#,
        r#"<svg><![CDATA[<script>alert(1)</script>]]></svg>"#,
        r#"<svg><!--<script>alert(1)</script>--></svg>"#,
        r#"<svg xmlns:x="http://www.w3.org/2000/svg"><x:script>alert(1)</x:script></svg>"#,
        r#"<svg><handler xmlns="http://www.w3.org/2001/xml-events">alert(1)</handler></svg>"#,
        r#"<svg><text><![CDATA[</text><script>alert(1)</script>]]></text></svg>"#,
    ];

    let deep = format!(
        "<svg>{}<script>alert(1)</script>{}</svg>",
        "<g>".repeat(200),
        "</g>".repeat(200)
    );

    for input in CORPUS.iter().copied().chain(std::iter::once(deep.as_str())) {
        // A refusal is a pass: nothing is served at all.
        let Ok(out) = sanitize_svg(input.as_bytes()) else {
            continue;
        };
        let out = String::from_utf8_lossy(&out).to_ascii_lowercase();
        assert!(!out.contains("<script"), "{input:?} → {out:?}");
        assert!(!out.contains("alert"), "{input:?} → {out:?}");
        assert!(!out.contains("onload"), "{input:?} → {out:?}");
        assert!(!out.contains("onbegin"), "{input:?} → {out:?}");
        assert!(!out.contains("foreignobject"), "{input:?} → {out:?}");
        assert!(!out.contains("<use"), "{input:?} → {out:?}");
        assert!(!out.contains("<image"), "{input:?} → {out:?}");
        assert!(!out.contains("<iframe"), "{input:?} → {out:?}");
        assert!(!out.contains("evil"), "{input:?} → {out:?}");
        assert!(!out.contains("javascript:"), "{input:?} → {out:?}");
        assert!(!out.contains("style"), "{input:?} → {out:?}");
        assert!(!out.contains("href"), "{input:?} → {out:?}");
    }
}

/// A `url(…)` reference is decided on the *compacted* value and emitted as it
/// was written, so what the output looks like and what `safe_value` read are not
/// the same string.
///
/// Pinned because the fuzz target's oracle has to agree with this. A
/// `starts_with('#')` on the raw output called `fill="url( #g)"` a bypass — the
/// whitespace is legal CSS, `safe_value` compacts it away before deciding, and
/// the serialiser escapes a quote to `&apos;` on the way out. Both forms are
/// local references and both are kept; a genuine external one still goes.
#[test]
fn a_local_reference_survives_however_it_is_spelled() {
    let out =
        sanitize_svg(br#"<svg fill="url( #g)" stroke="url('#h')" color="url(#i)"><rect/></svg>"#)
            .expect("a well-formed svg");
    let out = String::from_utf8(out).expect("utf-8 out");
    assert!(out.contains("url( #g)"), "{out}");
    assert!(out.contains("#h"), "{out}");
    assert!(out.contains("url(#i)"), "{out}");

    // And the whitespace trick does not smuggle an external reference through.
    let out = sanitize_svg(br#"<svg fill="url( http://evil.test/x)"><rect/></svg>"#)
        .expect("a well-formed svg");
    let out = String::from_utf8(out).expect("utf-8 out");
    assert!(!out.contains("evil.test"), "{out}");
}

/// **One attribute per name in the output.**
///
/// The allow-list matches the *local* name, so the prefix is stripped before the
/// attribute is written back — and `version="1.1" inkscape:version="1.0.2"`, on
/// every file Inkscape has ever saved, came back out as two `version`
/// attributes. Duplicate attribute names violate XML's Unique Att Spec: a
/// browser refuses the whole document, so a benign proxied logo rendered as a
/// broken image. It survived the idempotence test because `with_checks(false)`
/// is precisely the setting that lets this parser read back what a browser will
/// not.
#[test]
fn a_namespaced_attribute_cannot_duplicate_the_one_it_shadows() {
    let out = sanitize_svg(
        br#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:inkscape="http://inkscape" version="1.1" inkscape:version="1.0.2"><rect/></svg>"#,
    )
    .expect("a well-formed svg");
    let out = String::from_utf8(out).expect("utf-8 out");
    assert_eq!(
        out.matches("version=").count(),
        1,
        "exactly one `version` attribute must be emitted: {out}"
    );
    // And it is the SVG one, not the editor's note that happens to share a
    // local name.
    assert!(out.contains(r#"version="1.1""#), "{out}");
}

/// Order does not decide it: the un-prefixed spelling wins whichever came first.
#[test]
fn the_unprefixed_spelling_wins_whatever_the_order() {
    let out = sanitize_svg(
        br#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:inkscape="http://inkscape" inkscape:version="1.0.2" version="1.1"><rect/></svg>"#,
    )
    .expect("a well-formed svg");
    let out = String::from_utf8(out).expect("utf-8 out");
    assert_eq!(out.matches("version=").count(), 1, "{out}");
    assert!(out.contains(r#"version="1.1""#), "{out}");
}
