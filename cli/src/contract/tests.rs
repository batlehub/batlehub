//! RFC 0011 §10, the contract-file half.
//!
//! The properties here are the ones a second implementation — the editor
//! patch — depends on and cannot check for itself: that a rewrite preserves
//! what it did not write, that a failure to resolve is a "no credential"
//! rather than an error, and that nothing on the reporting path can carry a
//! secret.

use super::*;

use std::io::Write;

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().expect("temp dir")
}

fn write(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
    let p = dir.path().join(name);
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(body.as_bytes()).unwrap();
    p
}

// ── the document ─────────────────────────────────────────────────────────────

/// The shorthand every hand-written file uses, and the only shape the editor
/// patch reads.
#[test]
fn a_plain_string_token_is_the_literal_source() {
    let doc: ContractFile = serde_json::from_str(
        r#"{"version":1,"registries":{"https://hub.example.dev":{"token":"abc","kind":"oidc"}}}"#,
    )
    .unwrap();
    let e = doc.entry("https://hub.example.dev").unwrap();
    assert!(matches!(&e.token, TokenSource::Literal(v) if v == "abc"));
    let r = e.resolve();
    assert_eq!(r.state, State::Ok);
    assert_eq!(r.token.as_deref(), Some("abc"));
}

/// A trailing slash is the difference between a login that writes an entry
/// and an editor that reads one.
#[test]
fn an_origin_is_matched_with_or_without_its_trailing_slash() {
    let mut doc = ContractFile::default();
    doc.set_entry(
        "https://hub.example.dev/",
        Entry::literal("abc", Kind::Oidc, None),
    );
    assert!(doc.entry("https://hub.example.dev").is_some());
    assert!(doc.entry("https://hub.example.dev/").is_some());
    assert_eq!(
        doc.registries.keys().next().map(String::as_str),
        Some("https://hub.example.dev")
    );
}

/// A consumer that dropped what it did not understand would silently undo the
/// writer that added it — which is how `version` would have to move for every
/// added field instead of only for a changed one.
#[test]
fn a_rewrite_preserves_unknown_fields_and_other_registries() {
    let dir = tmp();
    let path = write(
        &dir,
        "vsx-token.json",
        r#"{
          "version": 1,
          "futureTopLevel": {"kept": true},
          "registries": {
            "https://hub.other.dev": {
              "token": "other-secret",
              "kind": "pat",
              "futureEntryField": ["kept"]
            }
          }
        }"#,
    );

    let mut doc = ContractFile::try_load(&path).unwrap();
    doc.set_entry(
        "https://hub.example.dev",
        Entry::literal("mine", Kind::Oidc, None),
    );
    doc.save(&path).unwrap();

    let back: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(back["futureTopLevel"]["kept"], true);
    assert_eq!(
        back["registries"]["https://hub.other.dev"]["futureEntryField"][0],
        "kept"
    );
    assert_eq!(
        back["registries"]["https://hub.other.dev"]["token"], "other-secret",
        "another registry's credential is not this writer's to touch"
    );
    assert_eq!(
        back["registries"]["https://hub.example.dev"]["token"],
        "mine"
    );
}

#[test]
fn a_write_is_atomic_and_owner_only() {
    let dir = tmp();
    let path = dir.path().join("state").join("vsx-token.json");
    let mut doc = ContractFile::default();
    doc.set_entry(
        "https://hub.example.dev",
        Entry::literal("s", Kind::Pat, None),
    );
    doc.save(&path).unwrap();

    assert!(path.exists());
    // No temp file left behind: the rename is the publish.
    let strays: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("tmp"))
        .collect();
    assert!(strays.is_empty(), "{strays:?}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the file holds a credential");
        let dir_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
    }
}

/// It must never break an anonymous gallery: unparseable is "no credential",
/// said once, not an error that stops the editor.
#[test]
fn an_unparseable_file_reads_as_no_credential_rather_than_failing() {
    let dir = tmp();
    let path = write(&dir, "vsx-token.json", "{ this is not json");
    let doc = ContractFile::load(&path);
    assert!(doc.registries.is_empty());
    assert_eq!(doc.version, CONTRACT_VERSION);

    // The writer, though, refuses to *overwrite* what it could not read —
    // that would discard another registry's entry.
    assert!(ContractFile::try_load(&path).is_err());
}

#[test]
fn a_document_from_the_future_is_refused_rather_than_guessed_at() {
    let dir = tmp();
    let path = write(&dir, "vsx-token.json", r#"{"version":99,"registries":{}}"#);
    let err = ContractFile::try_load(&path).unwrap_err().to_string();
    assert!(err.contains("version 99"), "{err}");
}

#[test]
fn an_absent_or_empty_file_is_an_empty_contract() {
    let dir = tmp();
    assert!(ContractFile::try_load(&dir.path().join("nope.json"))
        .unwrap()
        .registries
        .is_empty());
    let path = write(&dir, "empty.json", "   \n");
    assert!(ContractFile::try_load(&path).unwrap().registries.is_empty());
}

// ── the sources ──────────────────────────────────────────────────────────────

#[test]
fn a_file_source_resolves_to_the_files_contents_trimmed() {
    let dir = tmp();
    let token = write(&dir, "token", "  secret-value\n");
    let e = Entry::from_file(token.to_string_lossy(), Kind::Kubernetes);
    let r = e.resolve();
    assert_eq!(r.state, State::Ok);
    assert_eq!(r.token.as_deref(), Some("secret-value"));
    assert!(e.source_label().starts_with("file "));
}

#[test]
fn a_file_source_can_point_into_json() {
    let dir = tmp();
    let token = write(&dir, "token.json", r#"{"status":{"token":"deep-secret"}}"#);
    let e = Entry {
        token: TokenSource::Object(SourceObject::File {
            path: token.to_string_lossy().into_owned(),
            format: Some("json".into()),
            pointer: Some("/status/token".into()),
        }),
        kind: Kind::Kubernetes,
        expires_at: None,
        refresh: None,
        extra: BTreeMap::new(),
    };
    assert_eq!(e.resolve().token.as_deref(), Some("deep-secret"));
}

/// Every way a `file` source can fail means one thing to the editor: no
/// header. None of them is an error, and each says why.
#[test]
fn every_file_failure_is_no_credential_and_names_itself() {
    let dir = tmp();
    let cases: Vec<(&str, String)> = vec![
        ("relative", "relative/path".to_owned()),
        (
            "absent",
            dir.path().join("not-there").to_string_lossy().into_owned(),
        ),
        ("a directory", dir.path().to_string_lossy().into_owned()),
        (
            "empty",
            write(&dir, "blank", "\n\n").to_string_lossy().into_owned(),
        ),
    ];
    for (what, path) in cases {
        let e = Entry::from_file(path, Kind::Kubernetes);
        let r = e.resolve();
        assert_eq!(r.state, State::Unset, "{what}");
        assert!(r.token.is_none(), "{what}");
        assert!(r.detail.is_some(), "{what}: the reason is reported once");
    }
}

#[test]
fn a_file_larger_than_a_credential_is_refused_without_being_read() {
    let dir = tmp();
    let big = write(
        &dir,
        "big",
        &"x".repeat((MAX_TOKEN_FILE_BYTES + 1) as usize),
    );
    let e = Entry::from_file(big.to_string_lossy(), Kind::Kubernetes);
    let r = e.resolve();
    assert_eq!(r.state, State::Unset);
    assert!(r.detail.unwrap().contains("cap"));
}

/// §4.1.2 rule 4: an unknown source is the safe direction — it reads as no
/// credential, so a later source can be added without a `version` bump and
/// without this build guessing at it.
#[test]
fn an_unknown_source_reads_as_no_credential_rather_than_being_guessed_at() {
    let doc: ContractFile = serde_json::from_str(
        r#"{"version":1,"registries":{"https://h":{"token":{"from":"exchange",
            "endpoint":"https://idp/token","audience":"batlehub"},"kind":"oidc"}}}"#,
    )
    .unwrap();
    let r = doc.entry("https://h").unwrap().resolve();
    assert_eq!(r.state, State::Unsupported);
    assert!(r.token.is_none());
    assert!(r.detail.unwrap().contains("inline"));
}

// ── expiry ───────────────────────────────────────────────────────────────────

/// An expired entry is not a deleted entry: the owner can still refresh it,
/// and a consumer that removed it would log the user out of every registry
/// because it noticed first.
#[test]
fn an_expired_entry_still_resolves_and_says_it_is_expired() {
    let e = Entry::literal(
        "stale",
        Kind::Oidc,
        Some(Utc::now() - chrono::Duration::minutes(5)),
    );
    let r = e.resolve();
    assert_eq!(r.state, State::Expired);
    assert_eq!(
        r.token.as_deref(),
        Some("stale"),
        "the owner still has something to refresh"
    );
    assert_eq!(e.summary("https://h").expires_in, "expired");
}

#[test]
fn an_entry_with_no_expiry_is_treated_as_non_expiring() {
    let e = Entry::literal("pat", Kind::Pat, None);
    assert_eq!(e.resolve().state, State::Ok);
    assert_eq!(e.summary("https://h").expires_in, "—");
}

// ── what the CLI writes ──────────────────────────────────────────────────────

/// The CLI has a secret store, so it never writes `inline` refresh material
/// (§4.1.1 rule 1), and it names itself the owner of what it can redeem
/// (rule 2).
#[test]
fn the_cli_writes_the_refresh_descriptor_for_the_mode_it_logged_in_with() {
    let oidc = Entry::literal("t", Kind::Oidc, None);
    assert_eq!(oidc.refresh.as_ref().unwrap().source, RefreshSource::Cli);
    assert_eq!(
        oidc.refresh.as_ref().unwrap().owner.as_deref(),
        Some(CLI_OWNER)
    );

    let pat = Entry::literal("t", Kind::Pat, None);
    assert_eq!(pat.refresh.as_ref().unwrap().source, RefreshSource::None);

    let k8s = Entry::from_file("/var/run/secrets/x", Kind::Kubernetes);
    assert_eq!(
        k8s.refresh.as_ref().unwrap().source,
        RefreshSource::Reresolve,
        "something else keeps a projected token fresh"
    );

    for e in [oidc, pat, k8s] {
        assert!(
            !matches!(e.refresh.as_ref().unwrap().source, RefreshSource::Inline),
            "the CLI has a profile store; inline is for consumers that do not"
        );
    }
}

#[test]
fn every_refresh_source_and_kind_round_trips() {
    for source in [
        RefreshSource::Cli,
        RefreshSource::Reresolve,
        RefreshSource::Inline,
        RefreshSource::None,
    ] {
        let json = serde_json::to_string(&source).unwrap();
        let back: RefreshSource = serde_json::from_str(&json).unwrap();
        assert_eq!(back, source);
        assert!(json.contains(source.as_str()));
    }
    for kind in [Kind::Oidc, Kind::Pat, Kind::Kubernetes] {
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(serde_json::from_str::<Kind>(&json).unwrap(), kind);
        assert!(json.contains(kind.as_str()));
    }
}

// ── validation, at write time ────────────────────────────────────────────────

#[test]
fn a_reresolve_refresh_on_a_literal_token_is_refused() {
    let mut e = Entry::literal("t", Kind::Oidc, None);
    e.refresh = Some(Refresh {
        source: RefreshSource::Reresolve,
        owner: None,
        extra: BTreeMap::new(),
    });
    let err = validate_entry(&e).unwrap_err().to_string();
    assert!(err.contains("reresolve"), "{err}");
}

#[test]
fn a_file_source_is_refused_at_write_time_when_a_reader_could_not_act_on_it() {
    let mut e = Entry::from_file("relative/token", Kind::Kubernetes);
    assert!(validate_entry(&e)
        .unwrap_err()
        .to_string()
        .contains("absolute"));

    e = Entry {
        token: TokenSource::Object(SourceObject::File {
            path: "/abs/token".into(),
            format: Some("yaml".into()),
            pointer: None,
        }),
        ..e
    };
    assert!(validate_entry(&e).unwrap_err().to_string().contains("raw"));
}

#[test]
fn the_shapes_the_cli_writes_all_validate() {
    for e in [
        Entry::literal("t", Kind::Oidc, Some(Utc::now())),
        Entry::literal("t", Kind::Pat, None),
        Entry::from_file("/var/run/secrets/batlehub/token", Kind::Kubernetes),
    ] {
        validate_entry(&e).expect("what the CLI writes is what it accepts");
    }
}

// ── redaction ────────────────────────────────────────────────────────────────

/// Redaction as a property of the type: `EntrySummary` is what every status
/// path renders, and there is no field on it that can hold a credential — so
/// a log line added later cannot leak one.
#[test]
fn no_summary_field_can_carry_the_credential() {
    let dir = tmp();
    let token = write(&dir, "token", "super-secret-value");
    for e in [
        Entry::literal("super-secret-value", Kind::Oidc, Some(Utc::now())),
        Entry::from_file(token.to_string_lossy(), Kind::Kubernetes),
    ] {
        let s = e.summary("https://hub.example.dev");
        let rendered = serde_json::to_string(&s).unwrap();
        assert!(
            !rendered.contains("super-secret-value"),
            "the summary rendered a credential: {rendered}"
        );
        // …and the resolution that *does* carry it is a different type.
        assert_eq!(e.resolve().token.as_deref(), Some("super-secret-value"));
    }
}

// ── where the file lives ─────────────────────────────────────────────────────

#[test]
fn the_path_is_batlehub_home_state_and_the_env_var_overrides_it() {
    // One process, one environment: these two are asserted together rather
    // than in two tests that would race on it.
    let dir = tmp();
    temp_env(
        &[
            ("BATLEHUB_HOME", Some(dir.path().to_string_lossy().as_ref())),
            ("VSX_REGISTRY_AUTH_TOKEN_FILE", None),
        ],
        || {
            assert_eq!(
                contract_path(),
                dir.path().join("state").join("vsx-token.json")
            );
        },
    );
    temp_env(
        &[("VSX_REGISTRY_AUTH_TOKEN_FILE", Some("/tmp/elsewhere.json"))],
        || {
            assert_eq!(contract_path(), PathBuf::from("/tmp/elsewhere.json"));
        },
    );
}

/// Set, run, restore. `std::env::set_var` is process-wide, so the CLI's tests
/// keep the blast radius to one closure.
fn temp_env(vars: &[(&str, Option<&str>)], f: impl FnOnce()) {
    let saved: Vec<(String, Option<String>)> = vars
        .iter()
        .map(|(k, _)| ((*k).to_owned(), std::env::var(k).ok()))
        .collect();
    for (k, v) in vars {
        match v {
            Some(v) => std::env::set_var(k, v),
            None => std::env::remove_var(k),
        }
    }
    f();
    for (k, v) in saved {
        match v {
            Some(v) => std::env::set_var(&k, v),
            None => std::env::remove_var(&k),
        }
    }
}

/// The `json` format's own failure modes. They are "no credential" like every
/// other resolution failure, and each says which of the two things went wrong
/// — the file is not JSON, or the pointer names nothing — because those want
/// different fixes.
#[test]
fn a_json_file_source_that_does_not_yield_a_string_is_no_credential() {
    let dir = tmp();
    let not_json = write(&dir, "raw.txt", "just a token");
    let no_pointer = write(&dir, "doc.json", r#"{"other":"thing"}"#);
    let not_a_string = write(&dir, "num.json", r#"{"token": 42}"#);

    for (path, expect) in [
        (not_json, "not JSON"),
        (no_pointer, "no string at"),
        (not_a_string, "no string at"),
    ] {
        let e = Entry {
            token: TokenSource::Object(SourceObject::File {
                path: path.to_string_lossy().into_owned(),
                format: Some("json".into()),
                pointer: None,
            }),
            kind: Kind::Kubernetes,
            expires_at: None,
            refresh: None,
            extra: BTreeMap::new(),
        };
        let r = e.resolve();
        assert_eq!(r.state, State::Unset, "{path:?}");
        let detail = r.detail.unwrap();
        assert!(detail.contains(expect), "{detail}");
    }
}

/// `raw` is the default, and anything that is not `json` is read as raw
/// rather than refused at *read* time — the refusal happens where it can name
/// the writer, at validation.
#[test]
fn an_absent_format_reads_the_file_as_the_credential() {
    let dir = tmp();
    let token = write(
        &dir, "token", "plain
",
    );
    for format in [None, Some("raw".to_owned())] {
        let e = Entry {
            token: TokenSource::Object(SourceObject::File {
                path: token.to_string_lossy().into_owned(),
                format,
                pointer: None,
            }),
            kind: Kind::Kubernetes,
            expires_at: None,
            refresh: None,
            extra: BTreeMap::new(),
        };
        assert_eq!(e.resolve().token.as_deref(), Some("plain"));
    }
}

/// What the status table prints in the `Refresh` column. The owner is shown
/// because "who may redeem this" is the question the column exists to answer.
#[test]
fn the_refresh_column_names_the_source_and_its_owner() {
    assert_eq!(
        Entry::literal("t", Kind::Oidc, None).refresh_label(),
        "cli (batlehub-cli)"
    );
    assert_eq!(
        Entry::from_file("/var/run/x", Kind::Kubernetes).refresh_label(),
        "reresolve"
    );
    assert_eq!(Entry::literal("t", Kind::Pat, None).refresh_label(), "none");

    // An entry with no block at all reads as "none" rather than as blank:
    // absent and `"none"` mean the same thing (§4.1.1).
    let mut e = Entry::literal("t", Kind::Oidc, None);
    e.refresh = None;
    assert_eq!(e.refresh_label(), "none");
}

/// The two `validate_entry` cases that warn rather than refuse. Both are
/// writer bugs worth surfacing, and neither is a reason to refuse a
/// credential that would otherwise work.
#[test]
fn a_pat_with_a_refresh_block_and_an_inline_refresh_are_warned_about_not_refused() {
    let mut pat = Entry::literal("t", Kind::Pat, None);
    pat.refresh = Some(Refresh {
        source: RefreshSource::Cli,
        owner: Some("someone".into()),
        extra: BTreeMap::new(),
    });
    validate_entry(&pat).expect("a PAT that claims a refresh still works");

    let mut inline = Entry::literal("t", Kind::Oidc, None);
    inline.refresh = Some(Refresh {
        source: RefreshSource::Inline,
        owner: None,
        extra: BTreeMap::new(),
    });
    validate_entry(&inline).expect("inline refresh is a fallback, not an error");
}

/// The `Expires` column, which is the one an operator reads to decide whether
/// a credential is about to become their problem.
#[test]
fn the_expiry_column_reads_as_a_duration_at_every_scale() {
    // The units each scale renders in, not the exact figure: a second passes
    // between building the entry and rendering it, and a test that raced the
    // clock would be a flake rather than a check.
    fn units(s: &str) -> String {
        s.chars().filter(|c| !c.is_ascii_digit()).collect()
    }
    let cases = [
        (chrono::Duration::seconds(45), "s"),
        (chrono::Duration::seconds(252), "ms"),
        (chrono::Duration::minutes(58), "ms"),
        (chrono::Duration::hours(5), "hm"),
        (chrono::Duration::hours(51), "dh"),
    ];
    for (d, expect) in cases {
        let e = Entry::literal("t", Kind::Oidc, Some(Utc::now() + d));
        let got = e.summary("https://h").expires_in;
        assert_eq!(units(&got), expect, "{d:?} rendered as {got}");
    }
    // Past, and absent, are the two ends.
    assert_eq!(
        Entry::literal(
            "t",
            Kind::Oidc,
            Some(Utc::now() - chrono::Duration::seconds(1))
        )
        .summary("https://h")
        .expires_in,
        "expired"
    );
    assert_eq!(
        Entry::literal("t", Kind::Pat, None)
            .summary("https://h")
            .expires_in,
        "—"
    );
}

// ── the schema is the normative document ─────────────────────────────────────

/// RFC 0011 §4.1: "the normative schema is a JSON Schema shipped beside the
/// CLI, not this section … and the CLI's validation tests run against the
/// schema so the two cannot drift."
///
/// This is that check, and it runs in both directions — a vocabulary the
/// schema documents and the model cannot parse is a lie to the reader, and a
/// value the model accepts and the schema omits is a second implementation
/// written against the wrong document.
mod schema {
    use super::*;

    fn schema() -> Value {
        let raw = include_str!("../../schema/vsx-token.schema.json");
        serde_json::from_str(raw).expect("the shipped schema is valid JSON")
    }

    fn consts_of(node: &Value) -> Vec<String> {
        // `{"const": …}` directly, or a `oneOf` of them, or an `enum`.
        if let Some(c) = node.get("const").and_then(Value::as_str) {
            return vec![c.to_owned()];
        }
        if let Some(vals) = node.get("enum").and_then(Value::as_array) {
            return vals
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
        }
        node.get("oneOf")
            .and_then(Value::as_array)
            .map(|v| v.iter().flat_map(consts_of).collect())
            .unwrap_or_default()
    }

    #[test]
    fn the_schemas_version_is_the_one_this_build_writes() {
        let s = schema();
        assert_eq!(
            s["properties"]["version"]["const"].as_u64(),
            Some(CONTRACT_VERSION as u64)
        );
    }

    #[test]
    fn every_kind_the_schema_documents_is_one_the_model_parses() {
        let s = schema();
        let documented = consts_of(&s["$defs"]["entry"]["properties"]["kind"]);
        assert_eq!(documented.len(), 3, "{documented:?}");
        for k in &documented {
            serde_json::from_value::<Kind>(Value::String(k.clone())).unwrap_or_else(|e| {
                panic!("the schema documents kind `{k}` the model rejects: {e}")
            });
        }
        for k in [Kind::Oidc, Kind::Pat, Kind::Kubernetes] {
            assert!(
                documented.iter().any(|d| d == k.as_str()),
                "the model accepts kind `{}` the schema does not document",
                k.as_str()
            );
        }
    }

    #[test]
    fn every_refresh_source_the_schema_documents_is_one_the_model_parses() {
        let s = schema();
        let documented = consts_of(&s["$defs"]["refresh"]["properties"]["source"]);
        assert_eq!(documented.len(), 4, "{documented:?}");
        for r in &documented {
            serde_json::from_value::<RefreshSource>(Value::String(r.clone()))
                .unwrap_or_else(|e| panic!("the schema documents `{r}` the model rejects: {e}"));
        }
        for r in [
            RefreshSource::Cli,
            RefreshSource::Reresolve,
            RefreshSource::Inline,
            RefreshSource::None,
        ] {
            assert!(
                documented.iter().any(|d| d == r.as_str()),
                "the model accepts `{}` the schema does not document",
                r.as_str()
            );
        }
    }

    /// The reserved sources are the reason `version` does not have to move
    /// when one is implemented. The schema names them; this build must read
    /// each as "no credential" rather than guessing.
    #[test]
    fn every_reserved_source_reads_as_no_credential() {
        let s = schema();
        let reserved = consts_of(&s["$defs"]["reservedSource"]["properties"]["from"]);
        assert_eq!(reserved, vec!["env", "exchange", "keychain"]);
        for from in reserved {
            let entry: Entry = serde_json::from_value(serde_json::json!({
                "token": {"from": from, "path": "/x", "name": "X", "endpoint": "https://e"},
                "kind": "oidc"
            }))
            .unwrap_or_else(|e| panic!("a reserved source must still parse as an entry: {e}"));
            let r = entry.resolve();
            assert_eq!(r.state, State::Unsupported, "{from}");
            assert!(r.token.is_none(), "{from}");
        }
    }

    /// The implemented sources are documented with the fields the resolver
    /// actually reads, and the two shapes the shipped `file` reader supports.
    #[test]
    fn the_implemented_sources_are_documented_with_the_fields_the_resolver_reads() {
        let s = schema();
        let file = &s["$defs"]["fileSource"]["properties"];
        assert_eq!(file["from"]["const"], "file");
        assert_eq!(
            file["path"]["pattern"], "^/",
            "the schema states the absolute-path rule the resolver enforces"
        );
        assert_eq!(consts_of(&file["format"]), vec!["raw", "json"]);
        assert!(file["pointer"].is_object());

        let inline = &s["$defs"]["inlineSource"]["properties"];
        assert_eq!(inline["from"]["const"], "inline");
        assert!(inline["value"].is_object());
    }

    /// Every example in the schema is a document this build reads — the
    /// cheapest way for the two to disagree is an example nobody parses.
    #[test]
    fn every_example_the_schema_carries_parses_and_resolves() {
        let s = schema();
        let examples = s["examples"].as_array().expect("examples").clone();
        assert!(!examples.is_empty());
        for ex in examples {
            let doc: ContractFile = serde_json::from_value(ex.clone())
                .unwrap_or_else(|e| panic!("the schema's own example does not parse: {e}\n{ex:#}"));
            assert_eq!(doc.version, CONTRACT_VERSION);
            assert!(!doc.registries.is_empty());
            for (origin, entry) in &doc.registries {
                assert_eq!(
                    origin,
                    &normalize_origin(origin),
                    "{origin} is not normalized"
                );
                // Each resolves to *something*, and what it resolves to
                // depends on the machine reading it: `ok` for a literal,
                // `expired` for the one whose illustrative `expires_at` is
                // in the past, `unset` for the mounted file the example
                // names and this machine does not have. Never a panic, and
                // never an error — which is the property under test.
                assert!(
                    matches!(
                        entry.resolve().state,
                        State::Ok | State::Expired | State::Unset
                    ),
                    "{origin}"
                );
            }
        }
    }
}
