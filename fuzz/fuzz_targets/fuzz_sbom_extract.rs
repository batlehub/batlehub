#![no_main]
//! The SBOM extractors (`sbom::extractor`, RFC 0007 §5.2) — what reads a
//! manifest, a licence and a README out of a published archive: cargo's
//! `.crate`, npm's tarball, a wheel or sdist, a `.nupkg`, a Maven jar and its
//! `pom.xml`, a gem, a conda package, a Go module zip, a Composer zip, a
//! Terraform module.
//!
//! Every one of them takes bytes a publisher chose and answers on the
//! package page and in the licence gate. The contract is small and the same
//! for all ten: for any input — raw bytes, a real archive with fuzzed
//! members, a zip inside a tar — the call returns an `ExtractedManifest`,
//! never panics, and what it reports is bounded: no dependency name or
//! licence longer than an archive could plausibly carry, and the same input
//! answered the same way twice (the extractors are pure).

use std::io::Write;

use libfuzzer_sys::fuzz_target;

use batlehub_adapters::sbom::extractor::ArchiveSbomExtractor;
use batlehub_core::ports::SbomExtractor;

const KINDS: &[&str] = &[
    "cargo",
    "npm",
    "maven",
    "pypi",
    "nuget",
    "goproxy",
    "composer",
    "terraform",
    "conda",
    "rubygems",
    "generic",
];

/// File names the extractors look for, so a fuzzed archive has a chance of
/// reaching a parser rather than the "no manifest" branch.
const NAMES: &[&str] = &[
    "package/package.json",
    "package.json",
    "Cargo.toml",
    "cargo-0.1.0/Cargo.toml",
    "META-INF/MANIFEST.MF",
    "META-INF/maven/g/a/pom.xml",
    "pom.xml",
    "x-1.0.0.dist-info/METADATA",
    "x-1.0.0/PKG-INFO",
    "x/setup.py",
    "pyproject.toml",
    "x.nuspec",
    "_rels/.rels",
    "[Content_Types].xml",
    "metadata.gz",
    "data.tar.gz",
    "info/index.json",
    "info/about.json",
    "info/recipe/meta.yaml",
    "go.mod",
    "composer.json",
    "main.tf",
    "README.md",
    "readme.rst",
    "LICENSE",
    "LICENSE.txt",
];

/// Bodies with the shape each parser expects, plus noise.
const BODIES: &[&str] = &[
    r#"{"name":"x","version":"1.0.0","license":"MIT","dependencies":{"y":"^1"}}"#,
    "[package]\nname = \"x\"\nversion = \"1.0.0\"\nlicense = \"MIT\"\n[dependencies]\ny = \"1\"\n",
    "<project><groupId>g</groupId><artifactId>a</artifactId><version>1</version><licenses><license><name>MIT</name></license></licenses><dependencies><dependency><groupId>g</groupId><artifactId>b</artifactId><version>2</version></dependency></dependencies></project>",
    "Metadata-Version: 2.1\nName: x\nVersion: 1.0.0\nLicense: MIT\nRequires-Dist: y (>=1)\n",
    "<?xml version=\"1.0\"?><package><metadata><id>x</id><version>1.0.0</version><license type=\"expression\">MIT</license><dependencies><dependency id=\"y\" version=\"1.0\"/></dependencies></metadata></package>",
    "module x\n\ngo 1.22\n\nrequire y v1.0.0\n",
    "{\"name\":\"v/x\",\"license\":\"MIT\",\"require\":{\"v/y\":\"^1\"}}",
    "{\"name\":\"x\",\"version\":\"1.0.0\",\"license\":\"MIT\",\"depends\":[\"y >=1\"]}",
    "# Title\n\nsome readme\n",
    "MIT License\n",
    "",
];

fn member(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<(String, Vec<u8>)> {
    let name = if u.arbitrary::<bool>()? {
        NAMES[u.int_in_range(0..=NAMES.len() - 1)?].to_owned()
    } else {
        u.arbitrary::<String>()?
    };
    let body = match u.int_in_range(0..=2u8)? {
        0 => BODIES[u.int_in_range(0..=BODIES.len() - 1)?]
            .as_bytes()
            .to_vec(),
        1 => {
            let len = u.int_in_range(0..=2048usize)?;
            u.bytes(len)?.to_vec()
        }
        _ => {
            // A parser's own shape with fuzzed edits: a JSON value truncated,
            // a TOML key doubled, an XML tag left open.
            let base = BODIES[u.int_in_range(0..=BODIES.len() - 1)?].as_bytes();
            let cut = u.int_in_range(0..=base.len())?;
            let mut v = base[..cut].to_vec();
            let extra = u.int_in_range(0..=64usize)?;
            v.extend_from_slice(u.bytes(extra)?);
            v
        }
    };
    Ok((name, body))
}

fn tar_gz(members: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (name, data) in members {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_mtime(0);
        if h.set_path(name).is_ok() {
            h.set_cksum();
            let _ = b.append_data(&mut h, name, data.as_slice());
        }
    }
    let raw = b.into_inner().unwrap_or_default();
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let _ = enc.write_all(&raw);
    enc.finish().unwrap_or_default()
}

fn zip_of(members: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in members {
        if w.start_file(name.as_str(), opts).is_ok() {
            let _ = w.write_all(data);
        }
    }
    w.finish().map(|c| c.into_inner()).unwrap_or_default()
}

fuzz_target!(|data: &[u8]| {
    let mut u = arbitrary::Unstructured::new(data);
    let Ok(kind_idx) = u.int_in_range(0..=KINDS.len() - 1) else {
        return;
    };
    let kind = KINDS[kind_idx];
    let Ok(shape) = u.int_in_range(0..=3u8) else {
        return;
    };

    let mut members = Vec::new();
    if let Ok(n) = u.int_in_range(0..=6u8) {
        for _ in 0..n {
            match member(&mut u) {
                Ok(m) => members.push(m),
                Err(_) => break,
            }
        }
    }
    let archive: Vec<u8> = match shape {
        0 => u.bytes(u.len()).map(<[u8]>::to_vec).unwrap_or_default(),
        1 => tar_gz(&members),
        2 => zip_of(&members),
        _ => {
            // A gem: `metadata.gz` and `data.tar.gz` inside a plain tar — and
            // a nupkg/whl/jar is a zip, a crate/tgz/conda a tar.gz; the outer
            // shape is what the kind's parser sniffs first.
            let inner = tar_gz(&members);
            let mut b = tar::Builder::new(Vec::new());
            for name in ["metadata.gz", "data.tar.gz"] {
                let mut h = tar::Header::new_gnu();
                h.set_size(inner.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                let _ = b.append_data(&mut h, name, inner.as_slice());
            }
            b.into_inner().unwrap_or_default()
        }
    };

    let bytes = bytes::Bytes::from(archive);
    let first = ArchiveSbomExtractor.extract(&bytes, kind);
    let second = ArchiveSbomExtractor.extract(&bytes, kind);
    assert_eq!(
        format!("{first:?}"),
        format!("{second:?}"),
        "{kind}: the extractor is not pure"
    );
    for dep in &first.dependencies {
        assert!(
            dep.name.len() <= 4096,
            "{kind}: a dependency name of {} bytes",
            dep.name.len()
        );
    }
    if let Some(license) = &first.license {
        assert!(
            license.len() <= 4096,
            "{kind}: a licence of {} bytes",
            license.len()
        );
    }
});
