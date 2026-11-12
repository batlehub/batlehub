#![no_main]

//! RFC 0045 §6.1: whatever host and path reach the wire, a rewrite stays under
//! `/proxy/` and the sink under `/_air-gap/unmirrored/`, with no traversal and
//! no control character in either — the guard runs before any regex.

use libfuzzer_sys::fuzz_target;

use batlehub_core::entities::RegistryKind;
use batlehub_core::services::mise_wire::{WireOutcome, WireRegistry, WireTable};

fuzz_target!(|data: (&str, &str, bool)| {
    let (host, rest, air_gap) = data;
    let generic = vec![
        "https://storage.googleapis.com/acme-bucket".to_owned(),
        "http://mirror.lan:8081".to_owned(),
    ];
    let reg = |name, kind, upstream| WireRegistry {
        name,
        kind,
        upstream,
        upstreams: &[],
        path_routing: true,
    };
    let regs = [
        reg("gh", RegistryKind::Github, Some("https://api.github.com")),
        reg("fj", RegistryKind::Forgejo, Some("https://git.corp.example/forge")),
        reg("gl", RegistryKind::Gitlab, Some("https://gitlab.com")),
        reg("mv", RegistryKind::MiseVersions, None),
        reg("npm", RegistryKind::Npm, Some("https://registry.npmjs.org")),
        reg("cf", RegistryKind::Conda, Some("https://conda.anaconda.org/conda-forge")),
        WireRegistry {
            upstreams: &generic,
            ..reg("gen", RegistryKind::Generic, None)
        },
    ];
    let (table, _) = WireTable::build(&regs, Some("hub.example.com"));
    let clean = |p: &str| {
        !p.chars().any(|c| c.is_control() || c == '\\')
            && !p.split('/').any(|s| s == "." || s == "..")
    };
    match table.target(host, rest, air_gap) {
        WireOutcome::Rewrite(p) => {
            assert!(p.starts_with("/proxy/"), "{p}");
            assert!(clean(&p), "{p}");
        }
        WireOutcome::Sink(p) => {
            assert!(air_gap, "{p}");
            assert!(p.starts_with("/_air-gap/unmirrored/"), "{p}");
            assert!(clean(&p), "{p}");
        }
        WireOutcome::Bounce(u) => {
            assert!(!air_gap, "{u}");
            assert!(u.ends_with(&format!("://{host}/{rest}")), "{u}");
            assert!(clean(&u), "{u}");
        }
        WireOutcome::NotWired | WireOutcome::Malformed => {}
    }
});
