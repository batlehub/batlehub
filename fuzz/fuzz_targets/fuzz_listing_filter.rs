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

/// Every `DocumentKind` a filter can be asked for. Mismatched pairs (a NuGet
/// kind with a conda document) exercise the pass-through arms, which must
/// hold the same properties.
const DOCUMENTS: &[DocumentKind] = &[
    DocumentKind::Versions,
    DocumentKind::REGISTRATION,
    DocumentKind::GEM,
    DocumentKind::LATEST,
    DocumentKind::CURRENT_REPODATA,
    DocumentKind::P2_DEV,
    DocumentKind::SIMPLE_JSON,
    DocumentKind::COMPACT_VERSIONS,
    DocumentKind::COMPACT_INFO,
    DocumentKind::CHANNELDATA,
    DocumentKind::COMPACT_NAMES,
    DocumentKind::PROVIDER_DOWNLOAD,
    DocumentKind::INDEX_JSON,
    DocumentKind::SDKMAN_DEFAULT,
];

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
    let Ok(document) = pick(&mut u, DOCUMENTS) else {
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

    let Ok(is_json) = u.arbitrary::<bool>() else {
        return;
    };
    let doc = if is_json {
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
         (first pass removed {removed:?})"
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
