#![no_main]
//! The SVG allow-list, over arbitrary bytes.
//!
//! Two-thirds of the images in real READMEs are SVG (RFC 0007-bis §13.2), so
//! `remote_images = "proxy"` serves them — from the console's own origin, which
//! is the whole reason the sanitiser exists. Its HTML sibling
//! (`fuzz_readme_render`) got a fuzz target because the input is authored by
//! anyone who can publish to a proxied upstream; this input is authored by
//! anyone who can publish *and* by whoever runs the host a badge URL points at,
//! which is a strictly wider set.
//!
//! The invariant: whatever comes out is well-formed XML text, and carries no
//! script, no event handler and no reference to anything outside the document.
//! A **refusal** is a pass — nothing is served at all — so only successful
//! output is checked.
//!
//! Every check below is careful about *where* a match is, because a badge's own
//! label is text and its `d` attribute is a string: a package author may write
//! `xlink:href=` in either, and neither is markup. The first two runs of this
//! target reported exactly those as sanitiser bugs. Both were bugs in the check,
//! which is the same lesson `fuzz_readme_render` learned twice — and the
//! argument *for* these targets rather than against them, because a check that
//! cannot tell a name from a value would have passed a real bypass too.
//!
//! Two things this deliberately does not assert:
//!
//! - that the output is *valid* SVG. A sanitiser that emitted nonsense would be
//!   a bug, but not this bug, and a validity oracle would be a second XML
//!   implementation to be wrong in a different way.
//! - that anything survives. Emptying a hostile document is the correct answer;
//!   the corpus in `svg/tests.rs` is what asserts a real badge still reads.
//!
//! Run with `task fuzz TARGET=fuzz_svg_sanitize MAX_TIME=60`.

use libfuzzer_sys::fuzz_target;

use batlehub_core::services::svg::sanitize_svg;

fuzz_target!(|data: &[u8]| {
    let Ok(out) = sanitize_svg(data) else {
        // Refused outright: nothing reaches a browser, which is the safest
        // outcome there is.
        return;
    };

    // The output has to be UTF-8 — it is written by the serialiser, not copied
    // from the input — and anything else would mean a byte sequence escaped the
    // rewrite.
    let text = std::str::from_utf8(&out).expect("sanitised SVG must be UTF-8");
    let lower = text.to_ascii_lowercase();

    // Nothing this emits may claim to be XML and not be. `quick-xml` writes the
    // markup, so the risk is in what is copied through: a text node carrying a
    // byte XML 1.0 forbids would make the document unparseable, and a browser
    // that rejects `image/svg+xml` shows the reader a broken image rather than a
    // badge.
    for ch in text.chars() {
        assert!(
            !matches!(ch, '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}'),
            "a character XML forbids reached the output: {ch:?} in {text:?} (from {data:?})"
        );
    }

    // No element whose purpose is to execute or to fetch. Element names are
    // checked as written markup (`<name`), which text cannot produce: the
    // serialiser escapes `<` in a text node, so a `<` in the output is always a
    // tag.
    for forbidden in [
        "<script",
        "<foreignobject",
        "<iframe",
        "<use",
        "<image",
        "<style",
        "<animate",
        "<set",
        "<handler",
        "<a ",
    ] {
        assert!(
            !lower.contains(forbidden),
            "{forbidden} survived: {text:?} (from {data:?})"
        );
    }

    // Attributes and schemes need a *position* test rather than a plain
    // substring search, and it took two rounds to get right — the same two this
    // target's HTML sibling needed.
    //
    // Round one used a bare `contains`, and the fuzzer immediately produced a
    // badge label reading `xlink:href=`. Text, not markup: harmless.
    //
    // Round two tested "inside a tag", and the fuzzer produced `d="href=…"`.
    // Inside a tag, and inside a **quoted value**: also harmless, because a
    // value is a string and `href=` in one names nothing.
    //
    // Round three produced `font-style=""` — an attribute that **is** on the
    // allow-list, reported because `style=` is a substring of its name. A
    // position test alone cannot see that: the match sits inside a tag and
    // outside any value, so it looks exactly like a name. What was missing is
    // that it is not the *start* of one.
    //
    // What actually matters is the attribute-*name* position: inside a tag,
    // outside any value, and at a name boundary. The first two are exact
    // because the serialiser guarantees the escaping they rely on — `<` becomes
    // `&lt;` in text and `"` becomes `&quot;` in a value, so counting is safe.
    // The third deliberately does *not* lean on the serialiser; see below.
    let attribute_name_position = |idx: usize| {
        let before = &lower[..idx];
        let Some(tag_start) = before.rfind('<') else {
            return false;
        };
        if before.rfind('>').is_some_and(|gt| gt > tag_start) {
            return false; // between tags: text
        }
        // A match that continues a longer name is not a name. Stated as "the
        // previous character cannot be part of an XML name" rather than "must
        // be a space": the serialiser does write ` key="value"`, but asserting
        // on that would make this blind to an attribute emitted *without* its
        // space — `d="a"onload="x"` has even quote parity and would sail
        // through a whitespace test, which is the one shape a serialiser bug
        // would produce. `xlink:href=` is unaffected: it is on the list under
        // its own name, so it is caught with a space before it rather than as a
        // suffix of itself.
        if before
            .ends_with(|c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | ':'))
        {
            return false;
        }
        // An even number of quotes since `<` means every value opened has been
        // closed, so this position is where a name would go.
        before[tag_start..].matches('"').count() % 2 == 0
    };

    for attribute in [
        "href=",
        "xlink:href=",
        "style=",
        "src=",
        "srcset=",
        "onload=",
        "onbegin=",
    ] {
        for (idx, _) in lower.match_indices(attribute) {
            assert!(
                !attribute_name_position(idx),
                "attribute {attribute} survived: {text:?} (from {data:?})"
            );
        }
    }

    // A scheme *is* checked inside a value, because that is exactly where one
    // would execute — `fill="javascript:…"`. Only a text node is exempt.
    let inside_a_tag = |idx: usize| {
        let before = &lower[..idx];
        before.rfind('<') > before.rfind('>')
    };
    for scheme in ["javascript:", "vbscript:", "data:"] {
        for (idx, _) in lower.match_indices(scheme) {
            assert!(
                !inside_a_tag(idx),
                "{scheme} survived inside a tag: {text:?} (from {data:?})"
            );
        }
    }

    // `url(` may only ever be followed by a fragment — and only when it is in an
    // attribute value, for the same reason.
    //
    // Read the target the way `safe_value` reads it, not the way it is spelled.
    // The sanitiser compacts whitespace out and strips a surrounding quote before
    // deciding, then emits the value verbatim (with `'` escaped to `&apos;` by
    // the serialiser), so `fill="url( #g)"` and `stroke="url('#h')"` are accepted
    // and come out looking like they name something else. A `starts_with('#')` on
    // the raw text called both a bypass — a false positive on an input a badge
    // really writes, and the third of exactly this kind this target has had.
    for (idx, _) in lower.match_indices("url(") {
        if !inside_a_tag(idx) {
            continue;
        }
        let target: String = lower[idx + 4..]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let target = target
            .trim_start_matches("&apos;")
            .trim_start_matches("&quot;")
            .trim_start_matches(['\'', '"']);
        assert!(
            target.starts_with('#'),
            "url() in an attribute names something other than a fragment: {text:?} (from {data:?})"
        );
    }

    // Running it again must change nothing. A sanitiser whose output is not a
    // fixed point has a shape that means one thing on the first pass and another
    // on the second, which is the mXSS family in one sentence.
    let again = sanitize_svg(&out).expect("sanitised output must itself sanitise");
    assert_eq!(
        out, again,
        "sanitising is not idempotent: {text:?} (from {data:?})"
    );
});
