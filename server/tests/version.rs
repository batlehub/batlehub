//! What this build carries, and what every published build has to carry.
//!
//! The container images are `cargo build --release` with no `--features` of
//! their own, so the crate's `default` list *is* the feature set of everything
//! shipped. That list had no `storage-s3` in it while
//! `docs/guide/high-availability.md` told operators to configure
//! `[storage] type = "s3"` — a documented deployment that exits at startup
//! (`setup.rs`). The manifest test below is the barrier against that pairing
//! coming back; `--version` is how an operator asks a *running* binary the same
//! question, and it did not exist before (clap rejected the flag).

use std::process::Command;

#[test]
fn version_prints_the_package_version_and_the_feature_list() {
    let out = Command::new(env!("CARGO_BIN_EXE_batlehub"))
        .arg("--version")
        .output()
        .expect("run batlehub --version");

    assert!(
        out.status.success(),
        "--version exited with {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "--version does not name the package version: {stdout}"
    );
    assert!(
        stdout.contains("features:"),
        "--version does not list the compiled features: {stdout}"
    );
}

#[test]
fn version_names_storage_s3_exactly_when_it_is_compiled_in() {
    let out = Command::new(env!("CARGO_BIN_EXE_batlehub"))
        .arg("--version")
        .output()
        .expect("run batlehub --version");
    let stdout = String::from_utf8_lossy(&out.stdout);

    // The test binary and the binary under test are built from the same feature
    // resolution, so `cfg!` here is what the subprocess should report.
    assert_eq!(
        stdout.contains("storage-s3"),
        cfg!(feature = "storage-s3"),
        "--version disagrees with this build about storage-s3: {stdout}"
    );
}

/// Every storage and cache backend the configuration reference documents is a
/// default feature.
///
/// Read from the manifest rather than from `cfg!`, deliberately: `cfg!` reports
/// how *this* test was compiled, so it would pass a `--features storage-s3` run
/// of a crate that had dropped the feature from `default` — which is precisely
/// the build the images do not make.
#[test]
fn the_documented_backends_are_default_features() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("read server/Cargo.toml");
    // `Table`, not `Value`: since toml 1.0 a `Value`'s `FromStr` parses a single
    // value expression (`1`, `"a"`), not a whole document — it fails on the
    // first `[package]` with "unexpected content, expected nothing".
    let manifest: toml::Table = toml::from_str(&manifest).expect("parse server/Cargo.toml");
    let default = manifest["features"]["default"]
        .as_array()
        .expect("[features] default is a list");
    let default: Vec<&str> = default.iter().filter_map(|v| v.as_str()).collect();

    for feature in ["storage-s3", "cache-redis"] {
        assert!(
            default.contains(&feature),
            "`{feature}` is not a default feature, so the published images are built without it \
             and the backend it provides is refused at startup — while the docs still document \
             it. Either put it back, or stop documenting the backend. Defaults: {default:?}"
        );
    }
}
