use super::{Buf, Bytes, CargoDep, CargoIndexEntry};

// ── Sparse index proxy ────────────────────────────────────────────────────────

/// HTTP client + upstream index URL for one cargo sparse index.
#[derive(Clone)]
pub struct CargoIndexProxy {
    pub http: reqwest::Client,
    /// Base URL of the upstream sparse index, e.g. `https://index.crates.io`.
    pub index_url: String,
}

/// Parse the Cargo publish wire format:
/// `[4B LE u32 meta_len][JSON][4B LE u32 crate_len][.crate bytes]`
pub(super) fn parse_publish_body(mut body: Bytes) -> Result<(serde_json::Value, Bytes), String> {
    if body.remaining() < 4 {
        return Err("publish body too short (missing metadata length)".into());
    }
    let meta_len = body.get_u32_le() as usize;
    if body.remaining() < meta_len {
        return Err(format!(
            "metadata length {meta_len} exceeds remaining body ({} bytes)",
            body.remaining()
        ));
    }
    let meta_bytes = body.copy_to_bytes(meta_len);
    let meta_json: serde_json::Value = serde_json::from_slice(&meta_bytes)
        .map_err(|e| format!("invalid publish metadata JSON: {e}"))?;

    if body.remaining() < 4 {
        return Err("publish body too short (missing crate length)".into());
    }
    let crate_len = body.get_u32_le() as usize;
    if body.remaining() < crate_len {
        return Err(format!(
            "crate length {crate_len} exceeds remaining body ({} bytes)",
            body.remaining()
        ));
    }
    let crate_bytes = body.copy_to_bytes(crate_len);

    Ok((meta_json, crate_bytes))
}

/// Convert a Cargo publish metadata JSON object into a sparse index `CargoIndexEntry`.
/// The publish format uses `version_req`; the index format uses `req`.
pub(super) fn metadata_to_index_entry(
    meta: &serde_json::Value,
    checksum: &str,
) -> Result<CargoIndexEntry, String> {
    let name = meta["name"].as_str().ok_or("missing 'name'")?.to_owned();
    let vers = meta["vers"].as_str().ok_or("missing 'vers'")?.to_owned();

    let deps = meta
        .get("deps")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .map(|dep| {
                    Ok(CargoDep {
                        name: dep["name"].as_str().ok_or("dep missing 'name'")?.to_owned(),
                        req: dep
                            .get("version_req")
                            .and_then(|v| v.as_str())
                            .or_else(|| dep.get("req").and_then(|v| v.as_str()))
                            .ok_or("dep missing 'version_req'")?
                            .to_owned(),
                        features: dep
                            .get("features")
                            .and_then(|f| f.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(str::to_owned))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        optional: dep
                            .get("optional")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        default_features: dep
                            .get("default_features")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true),
                        target: dep
                            .get("target")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned),
                        kind: dep
                            .get("kind")
                            .and_then(|v| v.as_str())
                            .unwrap_or("normal")
                            .to_owned(),
                        registry: dep
                            .get("registry")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned),
                        explicit_name_in_toml: dep
                            .get("explicit_name_in_toml")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned),
                    })
                })
                .collect::<Result<Vec<_>, &str>>()
        })
        .transpose()
        .map_err(|e: &str| e.to_owned())?
        .unwrap_or_default();

    let features = meta
        .get("features")
        .cloned()
        .unwrap_or(serde_json::Value::Object(Default::default()));

    let links = meta
        .get("links")
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    let rust_version = meta
        .get("rust_version")
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    Ok(CargoIndexEntry {
        name,
        vers,
        deps,
        cksum: checksum.to_owned(),
        features,
        features2: None,
        yanked: false,
        links,
        rust_version,
        v: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::{BufMut, BytesMut};

    /// Build a valid Cargo publish wire-format body:
    /// `[4B LE meta_len][meta JSON][4B LE crate_len][crate bytes]`.
    fn build_body(meta: &serde_json::Value, crate_bytes: &[u8]) -> Bytes {
        let meta_bytes = serde_json::to_vec(meta).unwrap();
        let mut buf = BytesMut::new();
        buf.put_u32_le(meta_bytes.len() as u32);
        buf.put_slice(&meta_bytes);
        buf.put_u32_le(crate_bytes.len() as u32);
        buf.put_slice(crate_bytes);
        buf.freeze()
    }

    // ── parse_publish_body ──────────────────────────────────────────────────

    #[test]
    fn parse_publish_body_too_short_for_meta_len() {
        let body = Bytes::from_static(&[1, 2, 3]);
        let err = parse_publish_body(body).unwrap_err();
        assert!(err.contains("missing metadata length"), "{err}");
    }

    #[test]
    fn parse_publish_body_meta_len_exceeds_remaining() {
        let mut buf = BytesMut::new();
        buf.put_u32_le(100); // claims 100 bytes of metadata, but body ends here
        let err = parse_publish_body(buf.freeze()).unwrap_err();
        assert!(
            err.contains("metadata length 100 exceeds remaining body"),
            "{err}"
        );
    }

    #[test]
    fn parse_publish_body_invalid_meta_json() {
        let mut buf = BytesMut::new();
        let bad_json = b"not json";
        buf.put_u32_le(bad_json.len() as u32);
        buf.put_slice(bad_json);
        let err = parse_publish_body(buf.freeze()).unwrap_err();
        assert!(err.contains("invalid publish metadata JSON"), "{err}");
    }

    #[test]
    fn parse_publish_body_missing_crate_len() {
        let meta = serde_json::json!({"name": "foo", "vers": "1.0.0"});
        let meta_bytes = serde_json::to_vec(&meta).unwrap();
        let mut buf = BytesMut::new();
        buf.put_u32_le(meta_bytes.len() as u32);
        buf.put_slice(&meta_bytes);
        // no crate_len bytes follow
        let err = parse_publish_body(buf.freeze()).unwrap_err();
        assert!(err.contains("missing crate length"), "{err}");
    }

    #[test]
    fn parse_publish_body_crate_len_exceeds_remaining() {
        let meta = serde_json::json!({"name": "foo", "vers": "1.0.0"});
        let meta_bytes = serde_json::to_vec(&meta).unwrap();
        let mut buf = BytesMut::new();
        buf.put_u32_le(meta_bytes.len() as u32);
        buf.put_slice(&meta_bytes);
        buf.put_u32_le(50); // claims 50 bytes of crate data, but body ends here
        let err = parse_publish_body(buf.freeze()).unwrap_err();
        assert!(
            err.contains("crate length 50 exceeds remaining body"),
            "{err}"
        );
    }

    #[test]
    fn parse_publish_body_valid_roundtrip() {
        let meta = serde_json::json!({"name": "foo", "vers": "1.0.0"});
        let crate_bytes = b"fake crate tarball";
        let body = build_body(&meta, crate_bytes);

        let (parsed_meta, parsed_crate) = parse_publish_body(body).unwrap();
        assert_eq!(parsed_meta, meta);
        assert_eq!(&parsed_crate[..], crate_bytes);
    }

    // ── metadata_to_index_entry ─────────────────────────────────────────────

    #[test]
    fn metadata_to_index_entry_missing_name() {
        let meta = serde_json::json!({"vers": "1.0.0"});
        let err = metadata_to_index_entry(&meta, "cksum").unwrap_err();
        assert_eq!(err, "missing 'name'");
    }

    #[test]
    fn metadata_to_index_entry_missing_vers() {
        let meta = serde_json::json!({"name": "foo"});
        let err = metadata_to_index_entry(&meta, "cksum").unwrap_err();
        assert_eq!(err, "missing 'vers'");
    }

    #[test]
    fn metadata_to_index_entry_dep_missing_name() {
        let meta = serde_json::json!({
            "name": "foo",
            "vers": "1.0.0",
            "deps": [{"version_req": "^1.0"}]
        });
        let err = metadata_to_index_entry(&meta, "cksum").unwrap_err();
        assert_eq!(err, "dep missing 'name'");
    }

    #[test]
    fn metadata_to_index_entry_dep_missing_version_req() {
        let meta = serde_json::json!({
            "name": "foo",
            "vers": "1.0.0",
            "deps": [{"name": "bar"}]
        });
        let err = metadata_to_index_entry(&meta, "cksum").unwrap_err();
        assert_eq!(err, "dep missing 'version_req'");
    }

    #[test]
    fn metadata_to_index_entry_dep_req_field_fallback() {
        let meta = serde_json::json!({
            "name": "foo",
            "vers": "1.0.0",
            "deps": [{"name": "bar", "req": "^2.0"}]
        });
        let entry = metadata_to_index_entry(&meta, "cksum").unwrap();
        assert_eq!(entry.deps.len(), 1);
        assert_eq!(entry.deps[0].req, "^2.0");
    }

    #[test]
    fn metadata_to_index_entry_minimal() {
        let meta = serde_json::json!({"name": "foo", "vers": "1.0.0"});
        let entry = metadata_to_index_entry(&meta, "abc123").unwrap();

        assert_eq!(entry.name, "foo");
        assert_eq!(entry.vers, "1.0.0");
        assert!(entry.deps.is_empty());
        assert_eq!(entry.cksum, "abc123");
        assert_eq!(entry.features, serde_json::json!({}));
        assert_eq!(entry.features2, None);
        assert!(!entry.yanked);
        assert_eq!(entry.links, None);
        assert_eq!(entry.rust_version, None);
        assert_eq!(entry.v, None);
    }

    #[test]
    fn metadata_to_index_entry_full() {
        let meta = serde_json::json!({
            "name": "foo",
            "vers": "1.2.3",
            "deps": [{
                "name": "serde",
                "version_req": "^1.0",
                "features": ["derive", "rc"],
                "optional": true,
                "default_features": false,
                "target": "cfg(unix)",
                "kind": "dev",
                "registry": "https://example.com/index",
                "explicit_name_in_toml": "serde_renamed",
            }],
            "features": {"default": ["std"]},
            "links": "libfoo",
            "rust_version": "1.75",
        });

        let entry = metadata_to_index_entry(&meta, "deadbeef").unwrap();

        assert_eq!(entry.name, "foo");
        assert_eq!(entry.vers, "1.2.3");
        assert_eq!(entry.cksum, "deadbeef");
        assert_eq!(entry.features, serde_json::json!({"default": ["std"]}));
        assert_eq!(entry.links.as_deref(), Some("libfoo"));
        assert_eq!(entry.rust_version.as_deref(), Some("1.75"));

        assert_eq!(entry.deps.len(), 1);
        let dep = &entry.deps[0];
        assert_eq!(dep.name, "serde");
        assert_eq!(dep.req, "^1.0");
        assert_eq!(dep.features, vec!["derive".to_string(), "rc".to_string()]);
        assert!(dep.optional);
        assert!(!dep.default_features);
        assert_eq!(dep.target.as_deref(), Some("cfg(unix)"));
        assert_eq!(dep.kind, "dev");
        assert_eq!(dep.registry.as_deref(), Some("https://example.com/index"));
        assert_eq!(dep.explicit_name_in_toml.as_deref(), Some("serde_renamed"));
    }
}

/// The spelling a local crate is stored and looked up under.
///
/// cargo requests the sparse index by the lowercased name, so a crate stored
/// as `MyCrate` was one no dependent could resolve. The index entry keeps the
/// declared spelling; only the key is folded.
pub(super) fn local_crate_name(name: &str) -> String {
    name.to_ascii_lowercase()
}

/// The stored spelling of `name` among `existing`: a crate published as
/// `MyCrate` before names were folded keeps its row, so it is matched
/// case-insensitively; anything else gets the folded name.
pub(super) fn stored_crate_name(existing: &[String], name: &str) -> String {
    let folded = local_crate_name(name);
    if existing.contains(&folded) {
        return folded;
    }
    existing
        .iter()
        .find(|n| n.eq_ignore_ascii_case(name))
        .cloned()
        .unwrap_or(folded)
}

/// The stored spelling of `name` in the registry: the folded name, or the
/// mixed-case one a crate published before names were folded keeps (its
/// storage key is `{registry}/MyCrate/{version}`), or the folded name when
/// there is neither. One indexed lookup; call it after the auth/mode guards.
pub(super) async fn local_crate_key(
    local_svc: &batlehub_core::services::LocalRegistryService,
    registry: &str,
    name: &str,
) -> Result<String, crate::error::AppError> {
    let folded = local_crate_name(name);
    Ok(local_svc
        .backend
        .stored_name_ignore_case(registry, &folded)
        .await?
        .unwrap_or(folded))
}

/// crates.io's uniqueness rule: case-insensitive, with `-` and `_` equal.
pub(super) fn crate_names_collide(a: &str, b: &str) -> bool {
    let fold = |n: &str| n.to_ascii_lowercase().replace('_', "-");
    fold(a) == fold(b)
}

#[cfg(test)]
mod name_tests {
    use super::*;

    #[test]
    fn crate_names_fold_case_and_separators() {
        assert_eq!(local_crate_name("MyCrate"), "mycrate");
        assert!(crate_names_collide("foo-bar", "Foo_Bar"));
        assert!(!crate_names_collide("foo-bar", "foobar"));
    }

    #[test]
    fn a_crate_stored_before_folding_keeps_its_spelling() {
        let existing = vec!["MyCrate".to_owned(), "other".to_owned()];
        assert_eq!(stored_crate_name(&existing, "mycrate"), "MyCrate");
        assert_eq!(stored_crate_name(&existing, "MYCRATE"), "MyCrate");
        assert_eq!(stored_crate_name(&existing, "New"), "new");
        let both = vec!["MyCrate".to_owned(), "mycrate".to_owned()];
        assert_eq!(stored_crate_name(&both, "MyCrate"), "mycrate");
    }
}
