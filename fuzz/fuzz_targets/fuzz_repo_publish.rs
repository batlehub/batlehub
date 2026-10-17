#![no_main]
//! The system-package publish path: parse an upload, apply the handler's own
//! edge checks, and hold the file layout the result is stored under to the
//! properties the repository depends on.
//!
//! Each arm wraps fuzzed metadata in a **valid container** so the parsers get
//! past the framing to the part that matters — a hand-rolled `ar` with a plain
//! `control.tar` for deb, a plain tar for pacman, gzip over tar for apk — and
//! raw bytes for rpm, which is a crash-only arm (its header format is the
//! `rpm` crate's to parse). Everything accepted then goes through exactly what
//! `handlers/proxy/repo/publish.rs` calls before it writes, and must satisfy:
//!
//! - **One segment where the layout expects one.** The deb pool path is
//!   `pool/{component}/{prefix}/{name}/{file}` — five segments, none empty, `.`
//!   or `..`; the pacman file and DB directory, the apk file and the rpm
//!   storage id carry no `/` at all. A `/` in any of them nests the file in a
//!   directory another package's files share, or re-cuts the sidecar key
//!   `regenerate_*` groups on.
//! - **Injective file layout.** Two accepted coordinates that land on the same
//!   stored file are one coordinate: the second publish would otherwise replace
//!   the first's bytes under its own ownership check. Deb's one exception is
//!   the epoch, which Debian strips from pool file names; the handler refuses
//!   the second of two such versions against the stored sidecars, so here the
//!   property is stated modulo the epoch. Rpm's is the same: `storage_id`
//!   prefixes a non-zero epoch, so `(389, ds-base)` and `(0, 389-ds-base)` are
//!   one id and the handler refuses the second against the stored sidecar.
//! - **apk round trips.** The stored file name parses back (`apk_coordinate`)
//!   to the coordinate it was stored for, and the rendered index entry parses
//!   back (`ApkIndex::parse`) to the same `P:`/`V:`/`t:` — one block, nothing
//!   injected into it.

use std::io::Write;

use arbitrary::{Arbitrary, Result, Unstructured};
use libfuzzer_sys::fuzz_target;

use batlehub_adapters::repo::{apk, deb, pacman, rpm};
use batlehub_core::services::apk::{
    apk_coordinate, check_publish_coordinate, index_entry, render_index, ApkIndex,
};
use batlehub_core::services::validate_path_segment;

const TOKENS: &[&str] = &[
    "a", "b", "1", "1.0", "/", "-", "_", ":", ".", "-r0", "-1", "r1", "x86_64", "amd64", "all",
    "main", "\n", " ", "=", "%2F",
];

fn piece(u: &mut Unstructured) -> Result<String> {
    let mut s = String::new();
    for _ in 0..u.int_in_range(1..=4)? {
        if u.ratio(1, 8)? {
            s.push_str(&String::arbitrary(u)?);
        } else {
            s.push_str(u.choose(TOKENS)?);
        }
    }
    Ok(s)
}

/// A tar holding one file.
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

/// A BSD/GNU `ar` archive, written by hand: `!<arch>\n`, then per member a
/// 60-byte header and the body padded to an even length.
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

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(data).expect("in-memory gzip");
    enc.finish().expect("in-memory gzip")
}

/// The fields every arm fuzzes, plus raw extra lines for the rest of the file.
#[derive(Debug)]
struct Meta {
    name: String,
    version: String,
    arch: Option<String>,
    extra: String,
}

impl Meta {
    fn arbitrary(u: &mut Unstructured) -> Result<Self> {
        Ok(Self {
            name: piece(u)?,
            version: piece(u)?,
            arch: if u.ratio(7, 8)? {
                Some(piece(u)?)
            } else {
                None
            },
            extra: if u.ratio(1, 4)? {
                String::arbitrary(u)?
            } else {
                String::new()
            },
        })
    }

    fn render(&self, sep: &str, keys: [&str; 3]) -> String {
        let mut s = format!(
            "{}{sep}{}\n{}{sep}{}\n",
            keys[0], self.name, keys[1], self.version
        );
        if let Some(arch) = &self.arch {
            s.push_str(&format!("{}{sep}{arch}\n", keys[2]));
        }
        s.push_str(&self.extra);
        s
    }
}

// ── deb ─────────────────────────────────────────────────────────────────────

/// A `.deb` accepted by `deb_publish`'s edge, with where it would go.
struct DebPlaced {
    component: String,
    name: String,
    version: String,
    arch: String,
    pool: String,
    sidecar: String,
}

fn deb_place(distribution: &str, component: &str, pkg: &deb::DebPackage) -> Option<DebPlaced> {
    deb::check_coordinate(distribution, component, pkg).ok()?;
    let (name, version) = (pkg.name()?.to_owned(), pkg.version()?.to_owned());
    let arch = pkg.architecture().unwrap_or("all").to_owned();
    let pool = deb::pool_path(component, pkg).expect("checked coordinate has a pool path");
    let sidecar = format!("{distribution}/{component}/{arch}/{name}_{version}.stanza");
    Some(DebPlaced {
        component: component.to_owned(),
        name,
        version,
        arch,
        pool,
        sidecar,
    })
}

fn deb_arm(u: &mut Unstructured) -> Result<()> {
    let (distribution, component) = (piece(u)?, piece(u)?);
    let meta = Meta::arbitrary(u)?;
    let control = meta.render(": ", ["Package", "Version", "Architecture"]);
    let control_tar = tar_with("./control", control.as_bytes());
    let bytes = ar_with(&[("debian-binary", b"2.0\n"), ("control.tar", &control_tar)]);

    let Ok(pkg) = deb::parse_deb(&bytes) else {
        return Ok(());
    };
    let Some(a) = deb_place(&distribution, &component, &pkg) else {
        return Ok(());
    };

    let segments: Vec<&str> = a.pool.split('/').collect();
    assert_eq!(
        segments.len(),
        5,
        "pool path {:?} is not five segments",
        a.pool
    );
    assert_eq!(segments[1], a.component);
    assert!(
        segments
            .iter()
            .all(|s| !s.is_empty() && *s != "." && *s != ".."),
        "pool path {:?} has an empty or dot segment",
        a.pool
    );
    assert!(validate_path_segment("x", segments[4]).is_ok());
    // The sidecar key `regenerate_deb` cuts into (suite, component, arch, file).
    assert_eq!(a.sidecar.split('/').count(), 4, "sidecar {:?}", a.sidecar);

    // A second coordinate, built directly: the pool path and sidecar are
    // injective (pool modulo the epoch the archive strips).
    let other = Meta::arbitrary(u)?;
    let (dist_b, comp_b) = if u.arbitrary()? {
        (distribution.clone(), component.clone())
    } else {
        (piece(u)?, piece(u)?)
    };
    let mut control_b = vec![
        ("Package".to_owned(), other.name.trim().to_owned()),
        ("Version".to_owned(), other.version.trim().to_owned()),
    ];
    if let Some(arch) = &other.arch {
        control_b.push(("Architecture".to_owned(), arch.trim().to_owned()));
    }
    let pkg_b = deb::DebPackage {
        control: control_b,
        size: 0,
        sha256: String::new(),
    };
    if let Some(b) = deb_place(&dist_b, &comp_b, &pkg_b) {
        if a.pool == b.pool {
            assert_eq!(
                (
                    &a.component,
                    &a.name,
                    deb::file_version(&a.version),
                    &a.arch
                ),
                (
                    &b.component,
                    &b.name,
                    deb::file_version(&b.version),
                    &b.arch
                ),
                "two packages share the pool file {:?}",
                a.pool
            );
        }
        if a.sidecar == b.sidecar {
            assert_eq!(
                (&distribution, &a.component, &a.name, &a.version, &a.arch),
                (&dist_b, &b.component, &b.name, &b.version, &b.arch),
                "two packages share the sidecar {:?}",
                a.sidecar
            );
        }
    }
    Ok(())
}

// ── pacman ──────────────────────────────────────────────────────────────────

/// `{arch}/{file}` and the DB directory for a package `pacman_publish` accepts.
fn pacman_place(pkg: &pacman::PacmanPackage, suffix: &str) -> Option<(String, String, String)> {
    let (name, version, arch) = (pkg.name()?, pkg.version()?, pkg.arch()?);
    pacman::check_coordinate(name, version, arch).ok()?;
    let filename = format!("{name}-{version}-{arch}{suffix}");
    let dir = pacman::db_dir_name(pkg)?;
    Some((arch.to_owned(), filename, dir))
}

fn pacman_arm(u: &mut Unstructured) -> Result<()> {
    let meta = Meta::arbitrary(u)?;
    let pkginfo = meta.render(" = ", ["pkgname", "pkgver", "arch"]);
    let bytes = tar_with(".PKGINFO", pkginfo.as_bytes());
    let Ok(pkg) = pacman::parse_pacman(&bytes, "") else {
        return Ok(());
    };
    let Some((arch, file, dir)) = pacman_place(&pkg, pacman::download_suffix(&bytes)) else {
        return Ok(());
    };
    assert!(
        !file.contains('/') && !dir.contains('/'),
        "{file:?} / {dir:?}"
    );
    // `desc_entry` renders; it must not panic on what was accepted.
    let _ = pacman::desc_entry(&pkg);

    let other = Meta::arbitrary(u)?;
    let mut fields = vec![
        ("pkgname".to_owned(), other.name.trim().to_owned()),
        ("pkgver".to_owned(), other.version.trim().to_owned()),
    ];
    if let Some(a) = &other.arch {
        fields.push(("arch".to_owned(), a.trim().to_owned()));
    }
    let pkg_b = pacman::PacmanPackage {
        fields,
        filename: String::new(),
        csize: 0,
        sha256: String::new(),
        pgpsig: None,
    };
    if let Some((arch_b, file_b, dir_b)) = pacman_place(&pkg_b, ".pkg.tar") {
        let ids = |p: &pacman::PacmanPackage| {
            (p.name().map(str::to_owned), p.version().map(str::to_owned))
        };
        if format!("{arch}/{file}") == format!("{arch_b}/{file_b}")
            || (arch == arch_b && dir == dir_b)
        {
            assert_eq!(
                ids(&pkg),
                ids(&pkg_b),
                "two packages share {arch}/{file} or {dir}"
            );
        }
    }
    Ok(())
}

// ── apk ─────────────────────────────────────────────────────────────────────

fn apk_arm(u: &mut Unstructured) -> Result<()> {
    let meta = Meta::arbitrary(u)?;
    let mut pkginfo = meta.render(" = ", ["pkgname", "pkgver", "arch"]);
    let builddate: i64 = u.arbitrary()?;
    if u.arbitrary()? {
        pkginfo.push_str(&format!("\nbuilddate = {builddate}\n"));
    }
    let bytes = gzip(&tar_with(".PKGINFO", pkginfo.as_bytes()));
    let Ok(parsed) = apk::parse_apk(&bytes) else {
        return Ok(());
    };
    let info = &parsed.info;
    let (Some(name), Some(version), Some(arch)) = (info.name(), info.version(), info.arch()) else {
        return Ok(());
    };
    let Ok(filename) = check_publish_coordinate(name, version, arch) else {
        return Ok(());
    };
    // The handler's newline check, over every field.
    if info.fields.iter().any(|(_, v)| v.contains(['\n', '\r'])) {
        return Ok(());
    }

    assert!(!filename.contains('/'), "{filename:?}");
    assert_eq!(apk_coordinate(&filename), Some((name, version)));

    let text = render_index(&[index_entry(info, &parsed.identity, parsed.size)]);
    let index = ApkIndex::parse(&text);
    match info.first("builddate").and_then(|t| t.parse::<i64>().ok()) {
        Some(t) => {
            assert_eq!(index.built_at(name, version), Some(t), "index:\n{text}");
            assert_eq!(
                index.len(),
                1,
                "the entry parsed as more than one package:\n{text}"
            );
        }
        None => assert!(
            index.is_empty(),
            "a package with no build date was recorded:\n{text}"
        ),
    }

    // Injective `{arch}/{file}`: the file name parses back, so two coordinates
    // on one file are one coordinate.
    let other = Meta::arbitrary(u)?;
    let arch_b = other.arch.as_deref().unwrap_or("x86_64");
    if let Ok(file_b) = check_publish_coordinate(&other.name, &other.version, arch_b) {
        if (arch, filename.as_str()) == (arch_b, file_b.as_str()) {
            assert_eq!(
                (name, version),
                (other.name.as_str(), other.version.as_str())
            );
        }
    }
    Ok(())
}

// ── rpm ─────────────────────────────────────────────────────────────────────

fn rpm_arm(u: &mut Unstructured) -> Result<()> {
    // Crash-only on the header parser itself.
    let raw: Vec<u8> = u.arbitrary()?;
    if let Ok(pkg) = rpm::parse_rpm(&raw, "") {
        if rpm::check_coordinate(&pkg).is_ok() {
            assert!(!pkg.storage_id().contains('/'));
        }
    }

    // `storage_id` is injective between packages whose epochs are both zero or
    // both non-zero; the mixed case is the handler's sidecar check.
    let mut pkgs = Vec::new();
    for _ in 0..2 {
        let m = Meta::arbitrary(u)?;
        let p = rpm::RpmPackage {
            name: m.name,
            epoch: *u.choose(&[0, 0, 1, 389])?,
            version: m.version,
            release: piece(u)?,
            arch: m.arch.unwrap_or_else(|| "x86_64".to_owned()),
            ..Default::default()
        };
        if rpm::check_coordinate(&p).is_err() {
            return Ok(());
        }
        assert!(!p.storage_id().contains('/'), "{:?}", p.storage_id());
        pkgs.push(p);
    }
    let (a, b) = (&pkgs[0], &pkgs[1]);
    if a.storage_id() == b.storage_id() && (a.epoch == 0) == (b.epoch == 0) {
        let nevra = |p: &rpm::RpmPackage| {
            (
                p.name.clone(),
                p.epoch,
                p.version.clone(),
                p.release.clone(),
                p.arch.clone(),
            )
        };
        assert_eq!(
            nevra(a),
            nevra(b),
            "two packages share {:?}",
            a.storage_id()
        );
    }
    Ok(())
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let _ = match u.int_in_range(0..=3u8) {
        Ok(0) => deb_arm(&mut u),
        Ok(1) => pacman_arm(&mut u),
        Ok(2) => apk_arm(&mut u),
        Ok(_) => rpm_arm(&mut u),
        Err(_) => Ok(()),
    };
});
