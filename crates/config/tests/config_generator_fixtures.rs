//! The docs site's config generator, checked against the real loader.
//!
//! `docs/build/config-generator-fixtures.ts` renders each scenario of the
//! generator into `tests/fixtures/config-generator/*.toml`; this test loads
//! every one with `load_from_str` and runs `validate()`, so a section the
//! generator emits in a shape the server refuses is a red test here rather
//! than an operator's failed start. `task docs:generator:check` keeps the
//! fixtures in step with the generator.

use std::fs;
use std::path::PathBuf;

fn fixtures() -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config-generator");
    let mut out: Vec<(String, String)> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e} — run `task docs:generator`", dir.display()))
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "toml"))
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let body = fs::read_to_string(e.path()).expect("fixture is readable");
            (name, body)
        })
        .collect();
    out.sort();
    out
}

#[test]
fn every_generated_fixture_loads_and_validates() {
    let fixtures = fixtures();
    // A harness that scans nothing is worse than none.
    assert!(
        fixtures.len() >= 5,
        "only {} config-generator fixtures — run `task docs:generator`",
        fixtures.len()
    );
    for (name, body) in &fixtures {
        let cfg = batlehub_config::load_from_str(body)
            .unwrap_or_else(|e| panic!("{name}: the generator's TOML does not load: {e}"));
        cfg.validate()
            .unwrap_or_else(|e| panic!("{name}: the generator's TOML does not validate: {e}"));
    }
}

#[test]
fn the_vsx_signing_fixtures_carry_the_section_the_server_reads() {
    let fixtures = fixtures();
    let signed: Vec<_> = fixtures
        .iter()
        .filter(|(name, _)| name.contains("vsx-signing") && !name.contains("not-emitted"))
        .collect();
    assert_eq!(
        signed.len(),
        2,
        "{:?}",
        fixtures.iter().map(|f| &f.0).collect::<Vec<_>>()
    );
    for (name, body) in signed {
        let cfg = batlehub_config::load_from_str(body).unwrap();
        let reg = &cfg.registries[0];
        let signing = reg
            .vsx_signing
            .as_ref()
            .unwrap_or_else(|| panic!("{name}: no vsx_signing parsed"));
        assert_eq!(signing.seed_hex.len(), 64, "{name}");
    }
    let (_, body) = fixtures
        .iter()
        .find(|(name, _)| name.contains("not-emitted"))
        .expect("the npm scenario");
    let cfg = batlehub_config::load_from_str(body).unwrap();
    assert!(cfg.registries[0].vsx_signing.is_none());
}
