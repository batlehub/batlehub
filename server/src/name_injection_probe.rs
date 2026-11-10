//! Every registry kind's real client, aimed at a recorder, handed names,
//! versions and artifacts that carry `?` and `#` — what actix leaves in a path
//! parameter after decoding `%3F` / `%23`, and what `validate_path_safe` lets
//! through. A client that interpolates one raw sends the upstream a different
//! coordinate from the one authorization and the rules judged (inkdex
//! VULN-SSRF, PR #304, on the forges): the probe must reach the upstream whole,
//! inside its path, or not at all.
//!
//! The clients are the ones `build_registry_client` returns, behind
//! `CoordinateGuard`. Without it, 22 of 29 kinds failed this (333 requests).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use batlehub_core::entities::{PackageId, RegistryKind};
use batlehub_core::ports::{DocumentKind, RegistryClient};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Answers 404 to everything and keeps each request target.
async fn recorder() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let log = Arc::clone(&log);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16 * 1024];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]);
                if let Some(target) = head.lines().next().and_then(|l| l.split(' ').nth(1)) {
                    log.lock().unwrap().push(target.to_owned());
                }
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
            });
        }
    });
    (format!("http://{addr}"), seen)
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Each marker and the two whole probes that carry it.
const PROBES: &[(&str, [&str; 2])] = &[
    ("zzprobe", ["zzprobe?inj=1", "zzprobe#frag"]),
    ("zzver", ["zzver?inj=2", "zzver#frag"]),
    ("zzart", ["zzart?inj=3", "zzart#frag"]),
];

/// A request target is wrong when a probe's `?` reached the wire unencoded
/// (`inj=` in the raw query: a parameter of its own), or when a marker
/// arrives without the rest of its probe, in the path or in a query value
/// (the `#` probe: the fragment and whatever followed never leave the
/// client). A probe encoded inside a query value is the probe, whole.
fn misrouted(target: &str) -> bool {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let cut = |part: &str| {
        let part = decode(part);
        PROBES
            .iter()
            .any(|(m, whole)| part.contains(m) && !whole.iter().any(|w| part.contains(w)))
    };
    query.contains("inj=") || cut(path) || cut(query)
}

const DOCS: &[DocumentKind] = &[
    DocumentKind::Versions,
    DocumentKind::REGISTRATION,
    DocumentKind::GEM,
    DocumentKind::LATEST,
    DocumentKind::REPODATA_SHARDS,
    DocumentKind::CURRENT_REPODATA,
    DocumentKind::SIMPLE_JSON,
    DocumentKind::COMPACT_VERSIONS,
    DocumentKind::COMPACT_INFO,
    DocumentKind::CHANNELDATA,
    DocumentKind::COMPACT_NAMES,
    DocumentKind::PROVIDER_DOWNLOAD,
    DocumentKind::INDEX_JSON,
    DocumentKind::SDKMAN_DEFAULT,
    DocumentKind::SDKMAN_VERSIONS_LIST,
    DocumentKind::MANIFEST,
    DocumentKind::MANIFEST_ASC,
    DocumentKind::STABLE_DATE,
    DocumentKind::RUSTUP_RELEASE,
    DocumentKind::COLLECTION,
    DocumentKind::VERSION_DETAIL,
    DocumentKind::ROLE_VERSIONS,
    DocumentKind::ROLE,
    DocumentKind::NARINFO,
    DocumentKind::CACHE_INFO,
    DocumentKind::REALISATION,
    DocumentKind::BUILD_LOG,
    DocumentKind::RELAYED,
    DocumentKind::LEGACY_INDEX,
];

/// The shapes a name takes across kinds, so each client gets one it accepts.
fn shapes(p: &str) -> Vec<String> {
    vec![
        p.to_owned(),
        format!("zzo/{p}"),
        format!("org.zz:{p}"),
        format!("zzns.{p}"),
        format!("zzns/zzname/{p}"),
        format!("example.com/zz/{p}"),
    ]
}

async fn exercise(client: &dyn RegistryClient, pkg: PackageId) {
    let t = Duration::from_secs(3);
    let _ = tokio::time::timeout(t, client.resolve_metadata(&pkg)).await;
    let _ = tokio::time::timeout(t, client.fetch_artifact(&pkg)).await;
    let _ = tokio::time::timeout(t, client.list_versions(&pkg.name)).await;
    for d in DOCS {
        let _ = tokio::time::timeout(t, client.fetch_version_document(&pkg.name, *d)).await;
    }
}

fn registry(kind: &str, url: &str) -> batlehub_config::schema::RegistryConfig {
    #[derive(serde::Deserialize)]
    struct Wrapper {
        registries: Vec<batlehub_config::schema::RegistryConfig>,
    }
    let w: Wrapper = toml::from_str(&format!(
        "[[registries]]\ntype = \"{kind}\"\nname = \"reg\"\nupstreams = [\"{url}\"]\n"
    ))
    .expect("registry toml");
    w.registries.into_iter().next().unwrap()
}

/// Per kind: every misrouted request target, or why its client did not build.
async fn probe() -> Vec<(String, String)> {
    let mut findings = Vec::new();
    let mut total = 0;
    for kind in RegistryKind::ALL {
        let (url, seen) = recorder().await;
        let reg = registry(kind.as_str(), &url);
        let client = match super::builders::build_registry_client(&reg, None, None, false) {
            Ok(c) => c,
            Err(e) => {
                findings.push((kind.as_str().to_owned(), format!("build: {e}")));
                continue;
            }
        };
        for whole in PROBES[0].1 {
            for name in shapes(whole) {
                exercise(&*client, PackageId::new("reg", &name, "1.0.0")).await;
            }
        }
        for whole in ["9.9.9-zzver?inj=2", "9.9.9-zzver#frag"] {
            for name in shapes("zzname") {
                exercise(&*client, PackageId::new("reg", &name, whole)).await;
            }
        }
        for whole in ["zzart?inj=3.tgz", "zzart#frag.tgz"] {
            for name in shapes("zzname") {
                let pkg = PackageId {
                    artifact: Some(whole.to_owned()),
                    ..PackageId::new("reg", &name, "1.0.0")
                };
                exercise(&*client, pkg).await;
            }
        }
        let seen = seen.lock().unwrap().clone();
        let mut bad: Vec<String> = seen.iter().filter(|t| misrouted(t)).cloned().collect();
        bad.sort();
        bad.dedup();
        for t in bad {
            findings.push((kind.as_str().to_owned(), t));
        }
        eprintln!("{:24} requests seen: {}", kind.as_str(), seen.len());
        total += seen.len();
    }
    // The coordinates without a probe still reach the recorder: a client that
    // could not be exercised at all would pass this test for nothing.
    assert!(total > 500, "only {total} requests reached the recorder");
    findings
}

#[tokio::test(flavor = "multi_thread")]
async fn no_client_sends_a_probe_outside_its_own_path_segment() {
    let findings = probe().await;
    for (k, t) in &findings {
        eprintln!("FINDING {k:24} {t}");
    }
    assert!(
        findings.is_empty(),
        "{} misrouted request(s), see FINDING lines",
        findings.len()
    );
}
