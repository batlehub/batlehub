#![no_main]
//! The listing filters (RFC 0006): hiding a blocked version from what a client
//! is told exists, for every protocol at once.
//!
//! `services/blocking/` is one pure function per protocol over a document —
//! npm's packument, `maven-metadata.xml`, cargo's NDJSON, a PyPI simple page,
//! Go's `@v/list`, RubyGems' compact index — reached from the single
//! `dispatch`. Each has unit tests on documents someone wrote by hand. This
//! drives every `(kind, document)` pair with documents nobody wrote — token
//! soup for the text formats, a vocabulary-driven JSON tree for the rest, both
//! seeded with the same version pool the blocked set draws from, so a block
//! actually has something to hit.
//!
//! The properties are the ones every protocol shares, so the oracle owes
//! nothing to any one filter's reading of its format:
//!
//! 1. **An empty block set changes nothing.**
//! 2. **Only blocked versions are reported removed**, as the document spelled
//!    them: every entry of the returned list is in the blocked set after the
//!    protocol's own normalisation.
//! 3. **The body keeps its shape.** JSON stays JSON, text stays text, the
//!    content type and the `synthesised` marker are untouched — a filter must
//!    never answer a client's `Accept` with the other representation.
//! 4. **Filtering is idempotent.** Running the same filter over its own output
//!    removes nothing more and leaves the document byte-identical. This is the
//!    one that finds a loop that skips the element after the one it removed,
//!    a "repair `latest`" that picks differently on the second pass, or a
//!    trailing-newline normalisation that never settles.
//! 5. **Go's `@v/list` is clean.** The one format simple enough to state the
//!    end result directly: no surviving line is a blocked version.
//! 6. **npm, cargo and Maven are clean too**, on documents that are
//!    well-formed by construction: no blocked key survives in a packument's
//!    `versions` and no `dist-tags` entry names one; every sparse-index line
//!    whose `vers` is blocked is `yanked`; no `<version>`, `<latest>` or
//!    `<release>` of a `maven-metadata.xml` names one. Token soup almost never
//!    parses as these formats, so without a generator that builds the real
//!    shape the end-state of the three busiest protocols went unasserted.
//!
//! The document kind is drawn from `DocumentKind::ALL`, so a kind added in core
//! is fuzzed the day it lands rather than the day someone edits this list.
//!
//! `dispatch_multi` — conda's `repodata.json` and RubyGems' `/versions`, where
//! the blocked set is a registry's worth of `(package, version)` pairs — is
//! held to the same shape and idempotence properties.

use libfuzzer_sys::fuzz_target;
use serde_json::{Map, Value};

use batlehub_core::entities::RegistryKind;
use batlehub_core::ports::{DocumentBody, DocumentKind, VersionDocument};
use batlehub_core::services::blocking::{
    dispatch, dispatch_multi, BlockedVersions, ListingContext, MultiPackageBlocks,
};

/// Field names the filters look for, so a random object has a chance of being
/// read as a listing rather than skipped as noise.
const KEYS: &[&str] = &[
    "versions",
    "dist-tags",
    "latest",
    "time",
    "modified",
    "created",
    "items",
    "count",
    "lower",
    "upper",
    "catalogEntry",
    "version",
    "@id",
    "tag_name",
    "name",
    "num",
    "files",
    "filename",
    "url",
    "dist",
    "tarball",
    "packages",
    "modules",
    "urls",
    "Version",
    "Time",
    "Origin",
    "source",
    "dev",
    "subdirs",
    "info",
    "releases",
    "yanked",
    "vers",
    "number",
    "platform",
    "prerelease",
    "assets",
    "browser_download_url",
    "downloads",
    "channeldata_version",
    "repodata_version",
    "packages.conda",
    "build",
    "depends",
    "requires_python",
    "hashes",
    "sha256",
    "id",
    "type",
    "protocols",
    "shasum",
    "integrity",
    "sha",
];

/// Structural fragments of the text formats, joined at random with versions
/// from the pool.
const TEXT_TOKENS: &[&str] = &[
    "<metadata>",
    "</metadata>",
    "<versioning>",
    "</versioning>",
    "<versions>",
    "</versions>",
    "<version>",
    "</version>",
    "<latest>",
    "</latest>",
    "<release>",
    "</release>",
    "<lastUpdated>20240101000000</lastUpdated>",
    "<groupId>g</groupId>",
    "{\"name\":\"pkg\",\"vers\":\"",
    "\",\"yanked\":false,\"deps\":[]}",
    "\",\"yanked\":true}",
    "{\"vers\":\"",
    "\"}",
    "<a href=\"",
    "\">",
    "</a>",
    "pkg-",
    ".tar.gz",
    "-py3-none-any.whl",
    "#sha256=abc",
    "\n",
    "\r\n",
    "---\n",
    " |checksum:abc\n",
    " rack:>= 1.0|checksum:def\n",
    ",",
    "| Identifier",
    " | Vendor ",
    "================",
    "version\tdate\tfiles\tnpm\tv8\n",
    "v",
    " ",
    "\t",
    "|",
    "rack ",
    "created_at: 2024-01-01\n",
    "+incompatible",
    "-rc1",
    "<html><body>",
    "</body></html>",
    "\"",
    "{",
    "}",
    "[",
    "]",
    ":",
    "null",
    "true",
];

fn version(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<String> {
    Ok(match u.int_in_range(0..=4u8)? {
        0 => u.arbitrary::<String>()?,
        1 => format!(
            "{}.{}.{}",
            u.int_in_range(0..=20u8)?,
            u.int_in_range(0..=20u8)?,
            u.int_in_range(0..=20u8)?
        ),
        2 => format!(
            "v{}.{}.{}{}",
            u.int_in_range(0..=20u8)?,
            u.int_in_range(0..=20u8)?,
            u.int_in_range(0..=20u8)?,
            [
                "",
                "-rc1",
                "-SNAPSHOT",
                "+build.1",
                "rc1",
                "-beta.2",
                "+incompatible"
            ][u.int_in_range(0..=6usize)?]
        ),
        3 => format!(
            "{}.{}",
            u.int_in_range(0..=20u8)?,
            u.int_in_range(0..=20u8)?
        ),
        _ => format!(
            "{}.{}.{}.{}",
            u.int_in_range(0..=9u8)?,
            u.int_in_range(0..=9u8)?,
            u.int_in_range(0..=9u8)?,
            u.int_in_range(0..=99u8)?
        ),
    })
}

fn pick<'a, T>(u: &mut arbitrary::Unstructured<'_>, items: &'a [T]) -> arbitrary::Result<&'a T> {
    Ok(&items[u.int_in_range(0..=items.len() - 1)?])
}

fn json(
    u: &mut arbitrary::Unstructured<'_>,
    depth: u8,
    pool: &[String],
) -> arbitrary::Result<Value> {
    let leaf = depth == 0 || u.int_in_range(0..=3u8)? == 0;
    Ok(match u.int_in_range(0..=(if leaf { 4u8 } else { 6u8 }))? {
        0 => Value::Null,
        1 => Value::Bool(u.arbitrary()?),
        2 => Value::from(u.int_in_range(0..=1000u32)?),
        3 => Value::String(pick(u, pool)?.clone()),
        4 => Value::String(u.arbitrary()?),
        5 => {
            let mut items = Vec::new();
            for _ in 0..u.int_in_range(0..=4u8)? {
                items.push(json(u, depth - 1, pool)?);
            }
            Value::Array(items)
        }
        _ => {
            let mut map = Map::new();
            for _ in 0..u.int_in_range(0..=4u8)? {
                let key = match u.int_in_range(0..=2u8)? {
                    0 => (*pick(u, KEYS)?).to_owned(),
                    1 => pick(u, pool)?.clone(),
                    _ => u.arbitrary()?,
                };
                map.insert(key, json(u, depth - 1, pool)?);
            }
            Value::Object(map)
        }
    })
}

fn text(u: &mut arbitrary::Unstructured<'_>, pool: &[String]) -> arbitrary::Result<String> {
    let mut out = String::new();
    for _ in 0..u.int_in_range(0..=24u8)? {
        match u.int_in_range(0..=2u8)? {
            0 => out.push_str(pick(u, TEXT_TOKENS)?),
            1 => out.push_str(pick(u, pool)?),
            _ => out.push_str(&u.arbitrary::<String>()?),
        }
    }
    Ok(out)
}

/// A version a well-formed document can carry verbatim: XML text needs no
/// escaping and nothing trims it, so the oracle can read it back by slicing.
fn plain(v: &str) -> bool {
    !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
}

/// A packument, sparse index or `maven-metadata.xml` built from the pool, for
/// the three kinds property 6 states an end result for. `None` for every
/// other kind.
fn well_formed(
    u: &mut arbitrary::Unstructured<'_>,
    kind: RegistryKind,
    pool: &[String],
) -> arbitrary::Result<Option<VersionDocument>> {
    let listed: Vec<&String> = pool.iter().filter(|v| plain(v)).collect();
    Ok(Some(match kind {
        RegistryKind::Npm => well_formed_packument(u, &listed)?,
        RegistryKind::Cargo => well_formed_sparse_index(u, &listed)?,
        RegistryKind::Maven => well_formed_maven_metadata(u, &listed)?,
        _ => return Ok(None),
    }))
}

fn well_formed_packument(
    u: &mut arbitrary::Unstructured<'_>,
    listed: &[&String],
) -> arbitrary::Result<VersionDocument> {
    let mut versions = Map::new();
    let mut time = Map::new();
    for v in listed {
        if u.arbitrary()? {
            versions.insert(
                (*v).clone(),
                serde_json::json!({ "version": v, "dist": { "tarball": format!("https://r.invalid/pkg/-/pkg-{v}.tgz") } }),
            );
            time.insert((*v).clone(), Value::from("2024-01-01T00:00:00.000Z"));
        }
    }
    let keys: Vec<String> = versions.keys().cloned().collect();
    let mut tags = Map::new();
    if !keys.is_empty() {
        for tag in ["latest", "next", "beta"] {
            if u.arbitrary()? {
                tags.insert(tag.to_owned(), Value::from(pick(u, &keys)?.clone()));
            }
        }
    }
    Ok(VersionDocument::json(serde_json::json!({
        "name": "pkg",
        "dist-tags": tags,
        "versions": versions,
        "time": time,
    })))
}

fn well_formed_sparse_index(
    u: &mut arbitrary::Unstructured<'_>,
    listed: &[&String],
) -> arbitrary::Result<VersionDocument> {
    let mut body = String::new();
    for _ in 0..u.int_in_range(0..=8u8)? {
        if listed.is_empty() {
            break;
        }
        let line = serde_json::json!({
            "name": "pkg",
            "vers": pick(u, listed)?,
            "deps": [],
            "cksum": "00",
            "yanked": u.arbitrary::<bool>()?,
        });
        body.push_str(&line.to_string());
        body.push('\n');
    }
    Ok(VersionDocument::text("text/plain", body))
}

fn well_formed_maven_metadata(
    u: &mut arbitrary::Unstructured<'_>,
    listed: &[&String],
) -> arbitrary::Result<VersionDocument> {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<metadata>\n  \
         <groupId>g</groupId>\n  <artifactId>pkg</artifactId>\n  <versioning>\n",
    );
    // `<latest>`/`<release>` may name a version `<versions>` does not
    // list: Maven follows the pointer alone, so the filter has to
    // repair a blocked one either way (the first finding here).
    let mut kept = Vec::new();
    for v in listed {
        if u.arbitrary()? {
            kept.push(*v);
        }
    }
    if !listed.is_empty() {
        for el in ["latest", "release"] {
            if u.arbitrary()? {
                let v = pick(u, listed)?;
                xml.push_str(&format!("    <{el}>{v}</{el}>\n"));
            }
        }
    }
    xml.push_str("    <versions>\n");
    for v in &kept {
        xml.push_str(&format!("      <version>{v}</version>\n"));
    }
    xml.push_str(
        "    </versions>\n    <lastUpdated>20240101000000</lastUpdated>\n  \
         </versioning>\n</metadata>\n",
    );
    Ok(VersionDocument::text("text/xml", xml))
}

/// Property 6: the end state of a filtered npm, cargo or Maven document names
/// no blocked version where a client would select it.
fn assert_clean(kind: RegistryKind, doc: &VersionDocument, blocked: &BlockedVersions) {
    match (kind, &doc.body) {
        (RegistryKind::Npm, DocumentBody::Json(v)) => {
            if let Some(versions) = v.get("versions").and_then(Value::as_object) {
                for key in versions.keys() {
                    assert!(
                        !blocked.contains(key),
                        "packument still lists blocked {key:?}"
                    );
                }
            }
            if let Some(tags) = v.get("dist-tags").and_then(Value::as_object) {
                for (tag, target) in tags {
                    let t = target.as_str().unwrap_or_default();
                    assert!(
                        !blocked.contains(t),
                        "dist-tag {tag:?} still names blocked {t:?}"
                    );
                }
            }
        }
        (RegistryKind::Cargo, DocumentBody::Text(body)) => {
            for line in body.lines() {
                let Ok(v) = serde_json::from_str::<Value>(line) else {
                    panic!("the sparse index gained an unparseable line {line:?}");
                };
                let vers = v["vers"].as_str().unwrap_or_default();
                if blocked.contains(vers) {
                    assert_eq!(
                        v["yanked"],
                        Value::Bool(true),
                        "blocked {vers:?} is not yanked: {line}"
                    );
                }
            }
        }
        (RegistryKind::Maven, DocumentBody::Text(xml)) => {
            for el in ["version", "latest", "release"] {
                let (open, close) = (format!("<{el}>"), format!("</{el}>"));
                for (idx, _) in xml.match_indices(&open) {
                    let rest = &xml[idx + open.len()..];
                    let v = rest[..rest.find(&close).expect("unclosed element")].trim();
                    assert!(
                        v.is_empty() || !blocked.contains(v),
                        "<{el}> still names blocked {v:?} in {xml}"
                    );
                }
            }
        }
        _ => panic!("{kind}: the well-formed document changed representation"),
    }
}

fn same_shape(before: &VersionDocument, after: &VersionDocument) -> bool {
    before.content_type == after.content_type
        && before.synthesised == after.synthesised
        && matches!(
            (&before.body, &after.body),
            (DocumentBody::Json(_), DocumentBody::Json(_))
                | (DocumentBody::Text(_), DocumentBody::Text(_))
        )
}

fuzz_target!(|data: &[u8]| {
    let mut u = arbitrary::Unstructured::new(data);

    let Ok(kind) = pick(&mut u, RegistryKind::ALL) else {
        return;
    };
    let kind = *kind;
    // Every kind, including mismatched pairs (a NuGet kind with a conda
    // document): those exercise the pass-through arms, which must hold the same
    // properties.
    let Ok(document) = pick(&mut u, DocumentKind::ALL) else {
        return;
    };
    let document = *document;

    let Ok(pool_len) = u.int_in_range(1..=6usize) else {
        return;
    };
    let mut pool = Vec::with_capacity(pool_len);
    for _ in 0..pool_len {
        let Ok(v) = version(&mut u) else { return };
        pool.push(v);
    }

    let Ok(mode) = u.int_in_range(0..=2u8) else {
        return;
    };
    let mut well_formed_doc = None;
    if mode == 2 {
        match well_formed(&mut u, kind, &pool) {
            Ok(d) => well_formed_doc = d,
            Err(_) => return,
        }
    }
    let is_json = mode == 0;
    let doc = if let Some(d) = well_formed_doc.clone() {
        d
    } else if is_json {
        let Ok(value) = json(&mut u, 4, &pool) else {
            return;
        };
        VersionDocument::json(value)
    } else {
        let Ok(body) = text(&mut u, &pool) else {
            return;
        };
        let Ok(content_type) = pick(&mut u, &["text/xml", "text/html", "text/plain"]) else {
            return;
        };
        VersionDocument::text(*content_type, body)
    };

    // The blocked set: a subset of the pool, plus at most one spelling the
    // document never used.
    let mut blocked_versions = Vec::new();
    for v in &pool {
        if u.arbitrary::<bool>().unwrap_or(false) {
            blocked_versions.push(v.clone());
        }
    }
    if let Ok(true) = u.arbitrary::<bool>() {
        if let Ok(v) = version(&mut u) {
            blocked_versions.push(v);
        }
    }

    let ctx = ListingContext {
        registry: "reg",
        kind,
        document,
        package: "pkg",
        public_base: "https://proxy.example",
    };

    // 1. An empty block set changes nothing.
    let mut untouched = doc.clone();
    let none = BlockedVersions::new(kind, Vec::new());
    assert!(dispatch(&ctx, &mut untouched, &none).is_empty());
    assert_eq!(
        untouched, doc,
        "{kind}/{document}: an empty block set rewrote the document"
    );

    let blocked = BlockedVersions::new(kind, blocked_versions.clone());
    let mut once = doc.clone();
    let removed = dispatch(&ctx, &mut once, &blocked);

    // 2. Only blocked versions are reported removed.
    for r in &removed {
        assert!(
            blocked.contains(r),
            "{kind}/{document}: reported {r:?} removed, which is not blocked ({blocked_versions:?})"
        );
    }
    if blocked_versions.is_empty() {
        assert!(removed.is_empty());
        assert_eq!(once, doc);
    }

    // 3. The body keeps its shape.
    assert!(
        same_shape(&doc, &once),
        "{kind}/{document}: the filter changed the document's shape"
    );

    // 4. Idempotent.
    let mut twice = once.clone();
    let removed_again = dispatch(&ctx, &mut twice, &blocked);
    assert!(
        removed_again.is_empty(),
        "{kind}/{document}: a second pass removed {removed_again:?} the first pass left behind \
         (first pass removed {removed:?}) from {doc:?}, leaving {once:?}"
    );
    assert_eq!(
        twice, once,
        "{kind}/{document}: a second pass over a filtered document changed it"
    );

    // 5. Go's `@v/list`: no surviving line is a blocked version.
    if kind == RegistryKind::Goproxy && document == DocumentKind::Versions {
        if let DocumentBody::Text(body) = &once.body {
            for line in body.lines() {
                let v = line.trim();
                assert!(
                    v.is_empty() || !blocked.contains(v),
                    "@v/list still lists blocked {v:?}"
                );
            }
        }
    }

    // 6. npm, cargo and Maven: no blocked version survives where a client
    // would select it.
    if well_formed_doc.is_some() {
        assert_clean(kind, &once, &blocked);
    }

    // The multi-package entry point, for the kinds that have one.
    let names = ["pkg", "rack", "numpy", "other"];
    let mut pairs = Vec::new();
    for v in &blocked_versions {
        if let Ok(name) = pick(&mut u, &names) {
            pairs.push(((*name).to_owned(), v.clone()));
        }
    }
    let multi = MultiPackageBlocks::new(kind, pairs);
    let mut once_multi = doc.clone();
    let _removed = dispatch_multi(&ctx, &mut once_multi, &multi);
    assert!(
        same_shape(&doc, &once_multi),
        "{kind}/{document}: dispatch_multi changed the document's shape"
    );
    let mut twice_multi = once_multi.clone();
    let removed_again = dispatch_multi(&ctx, &mut twice_multi, &multi);
    assert!(
        removed_again.is_empty(),
        "{kind}/{document}: a second dispatch_multi pass removed {removed_again:?}"
    );
    assert_eq!(
        twice_multi, once_multi,
        "{kind}/{document}: a second dispatch_multi pass changed the document"
    );
    if multi.is_empty() {
        assert_eq!(once_multi, doc);
    }
});
