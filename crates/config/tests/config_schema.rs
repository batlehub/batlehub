//! The committed config JSON Schema files match the structs (RFC 0049 §6.3).
//!
//! `BLESS=1 cargo test -p batlehub-config --test config_schema` rewrites them
//! (`task docs:config-schema`). Only the value `1` blesses: a stray `BLESS`
//! in an environment must not turn the gate into a writer.

use std::path::PathBuf;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn schema_is_current() {
    let bless = std::env::var("BLESS").as_deref() == Ok("1");
    let mut stale = Vec::new();
    for (file, layer) in [
        ("config.schema.json", false),
        ("config-layer.schema.json", true),
    ] {
        let path = repo().join("docs/public/schema").join(file);
        let want = batlehub_config::json_schema_text(layer);
        if bless {
            std::fs::write(&path, &want).expect("writing the schema file");
            continue;
        }
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        // Compared as values: key order is not the gate's business.
        let parse = |s: &str| serde_json::from_str::<serde_json::Value>(s).ok();
        if parse(&have).is_none() || parse(&have) != parse(&want) {
            stale.push(format!("docs/public/schema/{file}"));
        }
    }
    assert!(
        stale.is_empty(),
        "{} out of date with crates/config/src/schema — run `task docs:config-schema` and commit",
        stale.join(" and ")
    );
}
