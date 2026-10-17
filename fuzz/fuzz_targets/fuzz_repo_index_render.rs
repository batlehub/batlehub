#![no_main]
//! The repository index renderers: what one upload contributes to the index
//! every client of the repository downloads.
//!
//! An index is shared. A package whose metadata breaks the document's syntax
//! does not break itself, it breaks the repository — one control character in
//! one summary and DNF refuses `primary.xml` for every package in it. So each
//! renderer is held to *parse back*, with an independent reader, into exactly
//! the fields it was given:
//!
//! - **rpm** `primary.xml`, `filelists.xml`, `other.xml` parse with quick-xml
//!   to the end, contain only characters XML 1.0 allows (quick-xml itself does
//!   not check that, so this does), and give back name, version, release and
//!   summary — attributes read with XML's attribute-value normalisation, text
//!   with its end-of-line normalisation, as libxml2 would. A character XML
//!   cannot carry is expected back as U+FFFD; everything else unchanged.
//! - **deb** every `Packages` stanza, joined by `generate_packages`, parses as
//!   deb822 into one paragraph per package holding the control fields it was
//!   rendered from (less the repository fields it recomputes), with no empty
//!   field name and no field twice.
//! - **pacman** the `desc` entry parses, the way `pacman`'s own db reader does,
//!   into `%FIELD%` sections that each appear once and carry the package's
//!   name, version, description and dependencies unchanged.

use arbitrary::{Arbitrary, Result, Unstructured};
use libfuzzer_sys::fuzz_target;
use quick_xml::events::Event;
use quick_xml::XmlVersion;

use batlehub_adapters::repo::{deb, pacman, rpm};

const TOKENS: &[&str] = &[
    "a", "1.0", ":", " ", "\t", "\n", "\r", "\r\n", "\n ", "\n.", "&", "<", ">", "\"", "]]>",
    "\u{1}", "\u{ffff}", "%NAME%", "#", "-", "Package", "Version", "Filename", "=",
];

fn piece(u: &mut Unstructured) -> Result<String> {
    let mut s = String::new();
    for _ in 0..u.int_in_range(0..=5)? {
        if u.ratio(1, 6)? {
            s.push_str(&String::arbitrary(u)?);
        } else {
            s.push_str(u.choose(TOKENS)?);
        }
    }
    Ok(s)
}

// ── rpm ─────────────────────────────────────────────────────────────────────

/// Whether XML 1.0 can carry `c` at all (§2.2 `Char`).
fn xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..)
}

/// What a value should read back as: unrepresentable characters replaced.
fn expected(s: &str) -> String {
    s.chars()
        .map(|c| if xml_char(c) { c } else { '\u{fffd}' })
        .collect()
}

/// Every `(element path, text)` and `(element, attribute, value)` in `xml`,
/// panicking on anything that is not well-formed.
fn read_xml(xml: &str) -> (Vec<(String, String)>, Vec<(String, String, String)>) {
    assert!(
        xml.chars().all(xml_char),
        "the document carries a non-XML character"
    );
    let mut reader = quick_xml::Reader::from_str(xml);
    let (mut texts, mut attrs) = (Vec::new(), Vec::new());
    let mut stack: Vec<(String, String)> = Vec::new();
    let mut record_attrs = |name: &str, e: &quick_xml::events::BytesStart| {
        for a in e.attributes() {
            let a = a.expect("well-formed attribute");
            let value = a
                .normalized_value(XmlVersion::Implicit1_0)
                .expect("attribute value unescapes");
            assert!(
                value.chars().all(xml_char),
                "a reference names a non-XML character"
            );
            attrs.push((
                name.to_owned(),
                a.key.as_ref().to_owned(),
                value.into_owned(),
            ));
        }
    };
    loop {
        match reader.read_event().expect("well-formed XML") {
            Event::Start(e) => {
                let name = e.name().as_ref().to_owned();
                record_attrs(&name, &e);
                stack.push((name, String::new()));
            }
            Event::Empty(e) => {
                let name = e.name().as_ref().to_owned();
                record_attrs(&name, &e);
            }
            Event::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    top.1.push_str(&t.xml10_content());
                }
            }
            Event::GeneralRef(r) => {
                let resolved = if r.is_char_ref() {
                    r.resolve_char_ref()
                        .expect("char ref")
                        .expect("char ref")
                        .to_string()
                } else {
                    quick_xml::escape::resolve_predefined_entity(&r.xml10_content())
                        .expect("predefined entity")
                        .to_owned()
                };
                assert!(
                    resolved.chars().all(xml_char),
                    "a reference names a non-XML character"
                );
                if let Some(top) = stack.last_mut() {
                    top.1.push_str(&resolved);
                }
            }
            Event::End(_) => {
                let (name, text) = stack.pop().expect("balanced");
                texts.push((name, text));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    assert!(stack.is_empty(), "unclosed elements");
    (texts, attrs)
}

fn rpm_arm(u: &mut Unstructured) -> Result<()> {
    let mut packages = Vec::new();
    for _ in 0..u.int_in_range(1..=2)? {
        packages.push(rpm::RpmPackage {
            name: piece(u)?,
            epoch: u.arbitrary()?,
            version: piece(u)?,
            release: piece(u)?,
            arch: piece(u)?,
            summary: piece(u)?,
            description: piece(u)?,
            license: piece(u)?,
            url: piece(u)?,
            location: piece(u)?,
            provides: vec![rpm::RpmDep {
                name: piece(u)?,
                flags: None,
                epoch: None,
                ver: Some(piece(u)?),
                rel: None,
            }],
            files: vec![rpm::RpmFile {
                path: piece(u)?,
                is_dir: u.arbitrary()?,
            }],
            ..Default::default()
        });
    }

    let (texts, attrs) = read_xml(&rpm::primary_xml(&packages));
    let names: Vec<&str> = texts
        .iter()
        .filter(|(n, _)| n == "name")
        .map(|(_, t)| t.as_str())
        .collect();
    let summaries: Vec<&str> = texts
        .iter()
        .filter(|(n, _)| n == "summary")
        .map(|(_, t)| t.as_str())
        .collect();
    let versions: Vec<(&str, &str)> = attrs
        .iter()
        .filter(|(el, _, _)| el == "version")
        .map(|(_, k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(names.len(), packages.len());
    for (i, p) in packages.iter().enumerate() {
        assert_eq!(names[i], expected(&p.name));
        assert_eq!(summaries[i], expected(&p.summary));
        let vers: Vec<&(&str, &str)> = versions.iter().skip(3 * i).take(3).collect();
        assert_eq!(vers[0], &("epoch", p.epoch.to_string().as_str()));
        assert_eq!(vers[1], &("ver", expected(&p.version).as_str()));
        assert_eq!(vers[2], &("rel", expected(&p.release).as_str()));
    }

    for doc in [rpm::filelists_xml(&packages), rpm::other_xml(&packages)] {
        let (_, attrs) = read_xml(&doc);
        let names: Vec<&str> = attrs
            .iter()
            .filter(|(el, k, _)| el == "package" && k == "name")
            .map(|(_, _, v)| v.as_str())
            .collect();
        let want: Vec<String> = packages.iter().map(|p| expected(&p.name)).collect();
        assert_eq!(names, want);
    }
    Ok(())
}

// ── deb ─────────────────────────────────────────────────────────────────────

fn tar_with(path: &str, body: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut tb = tar::Builder::new(&mut buf);
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        tb.append_data(&mut header, path, body)
            .expect("in-memory tar");
        tb.finish().expect("in-memory tar");
    }
    buf
}

fn ar_with(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = b"!<arch>\n".to_vec();
    for (name, body) in members {
        out.extend_from_slice(
            format!(
                "{name:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
                0,
                0,
                0,
                "100644",
                body.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(body);
        if body.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

/// deb822, read independently: paragraphs split on blank (or blank-looking)
/// lines, a leading space or tab continues the previous field, a field name is
/// everything before the first colon, its value trimmed of spaces and tabs.
fn deb822(text: &str) -> Vec<Vec<(String, String)>> {
    let mut paragraphs = vec![Vec::new()];
    for line in text.split('\n') {
        if line.trim_matches([' ', '\t']).is_empty() {
            if !paragraphs.last().expect("one").is_empty() {
                paragraphs.push(Vec::new());
            }
            continue;
        }
        let para: &mut Vec<(String, String)> = paragraphs.last_mut().expect("one");
        if line.starts_with([' ', '\t']) {
            let last = para.last_mut().expect("a continuation follows a field");
            last.1.push('\n');
            last.1.push_str(line);
            continue;
        }
        let (k, v) = line.split_once(':').expect("a field line has a colon");
        para.push((k.to_owned(), v.trim_matches([' ', '\t']).to_owned()));
    }
    paragraphs.retain(|p| !p.is_empty());
    paragraphs
}

fn deb_arm(u: &mut Unstructured) -> Result<()> {
    let mut pkgs = Vec::new();
    for _ in 0..u.int_in_range(1..=2)? {
        let mut control = String::new();
        for _ in 0..u.int_in_range(1..=4)? {
            control.push_str(&format!("{}: {}\n", piece(u)?, piece(u)?));
        }
        if u.ratio(1, 4)? {
            control.push_str(&String::arbitrary(u)?);
        }
        let bytes = ar_with(&[
            ("debian-binary", b"2.0\n"),
            ("control.tar", &tar_with("./control", control.as_bytes())),
        ]);
        if let Ok(pkg) = deb::parse_deb(&bytes) {
            pkgs.push(pkg);
        }
    }
    if pkgs.is_empty() {
        return Ok(());
    }
    let filename = "pool/main/a/a/a_1_all.deb";
    let stanzas: Vec<String> = pkgs
        .iter()
        .map(|p| deb::packages_stanza(p, filename))
        .collect();
    let paragraphs = deb822(&deb::generate_packages(&stanzas));
    assert_eq!(
        paragraphs.len(),
        pkgs.len(),
        "stanzas merged or split:\n{stanzas:?}"
    );

    for (pkg, para) in pkgs.iter().zip(&paragraphs) {
        for (i, (k, _)) in para.iter().enumerate() {
            assert!(!k.is_empty(), "an empty field name in {para:?}");
            assert!(
                !para[..i]
                    .iter()
                    .any(|(seen, _)| seen.eq_ignore_ascii_case(k)),
                "field {k:?} twice in {para:?}"
            );
        }
        let mut want: Vec<(String, String)> = pkg
            .control
            .iter()
            .filter(|(k, _)| {
                !matches!(
                    k.to_ascii_lowercase().as_str(),
                    "filename" | "size" | "md5sum" | "sha1" | "sha256"
                )
            })
            .cloned()
            .collect();
        want.push(("Filename".into(), filename.into()));
        want.push(("Size".into(), pkg.size.to_string()));
        want.push(("SHA256".into(), pkg.sha256.clone()));
        assert_eq!(para, &want);
    }
    Ok(())
}

// ── pacman ──────────────────────────────────────────────────────────────────

/// pacman's db reader: a `%FIELD%` line opens a section, every following line
/// up to a blank one is a value of it.
fn desc_sections(desc: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut open = false;
    for line in desc.split('\n') {
        if line.is_empty() {
            open = false;
        } else if open {
            out.last_mut().expect("open").1.push(line.to_owned());
        } else {
            let key = line
                .strip_prefix('%')
                .and_then(|l| l.strip_suffix('%'))
                .unwrap_or_else(|| panic!("{line:?} is neither a header nor a value:\n{desc}"));
            out.push((key.to_owned(), Vec::new()));
            open = true;
        }
    }
    out
}

fn pacman_arm(u: &mut Unstructured) -> Result<()> {
    let mut pkginfo = String::new();
    for key in ["pkgname", "pkgver", "pkgdesc", "depend", "depend", "arch"] {
        if u.ratio(3, 4)? {
            pkginfo.push_str(&format!("{key} = {}\n", piece(u)?));
        }
    }
    if u.ratio(1, 4)? {
        pkginfo.push_str(&String::arbitrary(u)?);
    }
    let bytes = tar_with(".PKGINFO", pkginfo.as_bytes());
    let Ok(pkg) = pacman::parse_pacman(&bytes, "hello-1-1-any.pkg.tar") else {
        return Ok(());
    };
    let desc = pacman::desc_entry(&pkg);
    let sections = desc_sections(&desc);
    for (i, (k, values)) in sections.iter().enumerate() {
        assert!(!values.is_empty(), "%{k}% has no value:\n{desc}");
        assert!(
            !sections[..i].iter().any(|(seen, _)| seen == k),
            "%{k}% twice:\n{desc}"
        );
    }
    let get = |key: &str| {
        sections
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    let first = |key: &str| {
        pkg.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .filter(|v| !v.is_empty())
    };
    assert_eq!(get("NAME"), first("pkgname").map(|v| vec![v]));
    assert_eq!(get("VERSION"), first("pkgver").map(|v| vec![v]));
    assert_eq!(get("DESC"), first("pkgdesc").map(|v| vec![v]));
    let depends: Vec<String> = pkg
        .fields
        .iter()
        .filter(|(k, v)| k == "depend" && !v.is_empty())
        .map(|(_, v)| v.clone())
        .collect();
    assert_eq!(get("DEPENDS").unwrap_or_default(), depends);
    Ok(())
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let _ = match u.int_in_range(0..=2u8) {
        Ok(0) => rpm_arm(&mut u),
        Ok(1) => deb_arm(&mut u),
        Ok(_) => pacman_arm(&mut u),
        Err(_) => Ok(()),
    };
});
