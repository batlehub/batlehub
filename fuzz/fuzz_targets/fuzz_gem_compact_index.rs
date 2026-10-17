#![no_main]
//! The RubyGems publish → compact-index path: `parse_gem_bytes` reads a
//! publisher-supplied `.gem`, the handler stores its fields, and
//! `LocalRegistryService::render_compact_info` writes them into `/info/{gem}`,
//! the document Bundler resolves from and trusts for checksums.
//!
//! `/info` is `---` then one line per live version:
//!
//! ```text
//! {version} {dep}:{req}&{req},{dep}:{req}|checksum:{sha256}
//! ```
//!
//! The oracle is an independent reader of that grammar, splitting on exactly
//! the delimiters Bundler splits on. For every set of gems the parser
//! *accepted*, the rendered document must read back as what was stored:
//!
//! 1. one line per non-yanked version, in order;
//! 2. each line's version and checksum are the stored ones;
//! 3. each line's dependencies are the parsed ones, name and requirement —
//!    which is only possible when no name carries one of ` ,:|\n` and no
//!    requirement one of `,|\n`. A field that does re-cuts the line and
//!    injects a checksum or a dependency the gem never declared.
//!
//! The gemspec is assembled from fuzzer-chosen strings dropped into the YAML
//! shape `gem build` writes, so a field can carry a delimiter, a quote or a
//! newline that starts a new YAML line, and the parser decides what survives.

use std::io::Write as _;

use libfuzzer_sys::fuzz_target;

use batlehub_adapters::registry::rubygems::parse_gem_bytes;
use batlehub_core::entities::PublishedPackage;
use batlehub_core::services::local_registry::LocalRegistryService;

#[derive(Debug, arbitrary::Arbitrary)]
struct Dep {
    name: String,
    op: String,
    version: String,
}

#[derive(Debug, arbitrary::Arbitrary)]
struct Gem {
    version: String,
    deps: Vec<Dep>,
    yanked: bool,
}

#[derive(Debug, arbitrary::Arbitrary)]
struct Input {
    name: String,
    gems: Vec<Gem>,
}

fn gemspec(name: &str, gem: &Gem) -> String {
    let mut y = format!(
        "--- !ruby/object:Gem::Specification\nname: {name}\nversion: !ruby/object:Gem::Version\n  version: '{}'\nplatform: ruby\n",
        gem.version
    );
    if !gem.deps.is_empty() {
        y.push_str("dependencies:\n");
        for d in &gem.deps {
            y.push_str(&format!(
                "- !ruby/object:Gem::Dependency\n  name: {}\n  requirement: !ruby/object:Gem::Requirement\n    requirements:\n    - - \"{}\"\n      - !ruby/object:Gem::Version\n        version: '{}'\n  type: :runtime\n",
                d.name, d.op, d.version
            ));
        }
    }
    y.push_str("summary: s\n");
    y
}

fn dot_gem(yaml: &str) -> Vec<u8> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(yaml.as_bytes()).unwrap();
    let metadata = gz.finish().unwrap();
    let mut tar = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(metadata.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, "metadata.gz", metadata.as_slice())
        .unwrap();
    tar.into_inner().unwrap()
}

/// What the oracle expects one `/info` line to say.
struct Expected {
    version: String,
    deps: Vec<(String, String)>,
    checksum: String,
}

/// An independent `/info` line reader: Bundler's split, nothing more.
fn read_line(line: &str) -> (String, Vec<(String, String)>, String) {
    let (version, rest) = line
        .split_once(' ')
        .unwrap_or_else(|| panic!("no space after the version: {line:?}"));
    let parts: Vec<&str> = rest.split('|').collect();
    assert_eq!(parts.len(), 2, "exactly one `|` per line: {line:?}");
    let checksum = parts[1]
        .strip_prefix("checksum:")
        .unwrap_or_else(|| panic!("the tail is not `checksum:`: {line:?}"));
    assert!(
        !checksum.contains(',') && !checksum.contains(':'),
        "one checksum field: {line:?}"
    );
    let deps = if parts[0].is_empty() {
        Vec::new()
    } else {
        parts[0]
            .split(',')
            .map(|dep| {
                let f: Vec<&str> = dep.split(':').collect();
                assert_eq!(
                    f.len(),
                    2,
                    "a dependency is `name:req`: {dep:?} in {line:?}"
                );
                (f[0].to_owned(), f[1].to_owned())
            })
            .collect()
    };
    (version.to_owned(), deps, checksum.to_owned())
}

fuzz_target!(|input: Input| {
    if input.gems.len() > 8 {
        return;
    }
    let mut stored = Vec::new();
    let mut expected = Vec::new();
    for (i, gem) in input.gems.iter().enumerate() {
        let Ok(meta) = parse_gem_bytes(&dot_gem(&gemspec(&input.name, gem))) else {
            continue;
        };
        // The field-level rule the property depends on, stated directly.
        for d in &meta.dependencies {
            assert!(
                !d.name.contains([' ', ',', ':', '|', '\n', '\r']),
                "accepted dependency name {:?}",
                d.name
            );
            assert!(
                !d.requirement.contains([',', '|', '\n', '\r']),
                "accepted requirement {:?}",
                d.requirement
            );
        }
        assert!(
            !meta.version.contains([' ', ',', ':', '|', '\n', '\r']),
            "accepted version {:?}",
            meta.version
        );
        let checksum = format!("{i:064x}");
        // As `gem_publish` stores it.
        let index_metadata = serde_json::json!({
            "name": meta.name,
            "version": meta.version,
            "platform": meta.platform,
            "sha": checksum,
            "dependencies": meta.dependencies.iter()
                .map(|d| serde_json::json!({ "name": d.name, "requirement": d.requirement }))
                .collect::<Vec<_>>(),
        });
        // An exhaustive literal: a field added later is a compile error here,
        // not a panic on every accepted input.
        let pkg = PublishedPackage {
            registry: "gems".to_owned(),
            name: meta.name.clone(),
            version: meta.version.clone(),
            checksum: checksum.clone(),
            yanked: gem.yanked,
            deprecated: false,
            deprecation_message: None,
            unlisted: false,
            index_metadata,
            published_at: chrono::DateTime::UNIX_EPOCH,
            published_by: None,
            signature_bytes: None,
            signature_type: None,
            visibility: Default::default(),
            retention_keep: false,
        };
        if !gem.yanked {
            expected.push(Expected {
                version: meta.version.clone(),
                deps: meta
                    .dependencies
                    .iter()
                    .map(|d| (d.name.clone(), d.requirement.clone()))
                    .collect(),
                checksum,
            });
        }
        stored.push(pkg);
    }

    let info = LocalRegistryService::render_compact_info(&stored);
    let body = info.strip_prefix("---\n").expect("the `---` header");
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(
        lines.len(),
        expected.len(),
        "one line per live version:\n{info}"
    );
    for (line, want) in lines.iter().zip(&expected) {
        let (version, deps, checksum) = read_line(line);
        assert_eq!(version, want.version, "{line:?}");
        assert_eq!(checksum, want.checksum, "{line:?}");
        assert_eq!(deps, want.deps, "{line:?}");
    }
});
