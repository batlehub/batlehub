//! RFC 0040 §4.9: the journeys the console had and the CLI did not.
//!
//! Each test runs the binary against a stand-in server that expects the one
//! request the command maps to — method, path, query and body — and answers
//! what the OpenAPI spec says it answers. What the server does with the call is
//! the web suites' to prove; this proves the CLI sends it, signs it, confirms
//! before it, and prints the answer in both formats.

use mockito::Matcher;
use serde_json::{json, Value};

const TOKEN: &str = "journey-token";

fn cli(args: &[&str], server: &str, env: &[(&str, &str)]) -> (i32, String, String) {
    let home = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_batlehub-cli"))
        .args(args)
        .env("BATLEHUB_SERVER", server)
        .env("BATLEHUB_TOKEN", TOKEN)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env_remove("BATLEHUB_REGISTRY")
        .env_remove("BATLEHUB_FORMAT")
        .envs(env.iter().copied())
        .output()
        .expect("run batlehub-cli");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A stand-in listing one npm registry `r`, so `r/…` resolves as a prefix.
fn server() -> mockito::ServerGuard {
    let mut s = mockito::Server::new();
    s.mock("GET", "/api/v1/registries")
        .with_body(r#"[{"name":"r","type":"npm","mode":"local"}]"#)
        .create();
    s
}

fn json_of(stdout: &str) -> Value {
    serde_json::from_str(stdout).unwrap_or_else(|e| panic!("{e}: {stdout}"))
}

// ── package block · unblock ──────────────────────────────────────────────────

#[test]
fn package_block_confirms_then_sends_one_request_for_every_registry() {
    let mut s = server();
    let block = s
        .mock("POST", "/api/v1/admin/packages/bulk-block")
        .match_body(Matcher::Json(json!({"items": [
            {"registry": "r", "name": "a", "version": "1.0.0", "reason": "malware"},
            {"registry": "r", "name": "b", "version": "2.0.0", "reason": "malware"},
        ]})))
        .with_body(
            r#"{"succeeded_count":1,"failed_count":1,"failures":[
                {"registry":"r","name":"b","version":"2.0.0","error":"not found"}]}"#,
        )
        .expect(1)
        .create();

    let args = [
        "package",
        "block",
        "r/a@1.0.0",
        "r/b@2.0.0",
        "--reason",
        "malware",
    ];
    let (code, _, stderr) = cli(&args, &s.url(), &[]);
    assert_eq!(code, 9, "no terminal and no --yes: {stderr}");

    let (code, stdout, stderr) = cli(
        &[&args[..], &["-y", "--format", "json"]].concat(),
        &s.url(),
        &[],
    );
    assert_eq!(code, 7, "one of two failed is a partial failure: {stderr}");
    let out = json_of(&stdout);
    assert_eq!(out["succeeded"], json!(["r/a@1.0.0"]));
    assert_eq!(out["failed"][0]["error"], "not found");
    block.assert();
}

#[test]
fn package_unblock_asks_nothing() {
    let mut s = server();
    let unblock = s
        .mock("POST", "/api/v1/admin/packages/bulk-unblock")
        .match_body(Matcher::Json(json!({"items": [
            {"registry": "r", "name": "a", "version": "1.0.0"}]})))
        .with_body(r#"{"succeeded_count":1,"failed_count":0,"failures":[]}"#)
        .create();
    let (code, stdout, stderr) = cli(&["package", "unblock", "r/a@1.0.0"], &s.url(), &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("Unblocked r/a@1.0.0"), "{stdout}");
    unblock.assert();
}

// ── policy · exemption ───────────────────────────────────────────────────────

#[test]
fn policy_show_set_and_delete_address_the_package_or_the_version() {
    let mut s = server();
    let show = s
        .mock("GET", "/api/v1/admin/registries/r/policy/version/a/1.0.0")
        .with_body(r#"{"visibility":"internal","rules":[]}"#)
        .create();
    let set = s
        .mock("PUT", "/api/v1/admin/registries/r/policy/package/a")
        .match_body(Matcher::Json(json!({"visibility": "team"})))
        .with_status(204)
        .create();
    let delete = s
        .mock("DELETE", "/api/v1/admin/registries/r/policy/package/a")
        .with_status(204)
        .create();

    let (code, stdout, _) = cli(&["policy", "show", "r/a@1.0.0"], &s.url(), &[]);
    assert_eq!(code, 0);
    assert!(stdout.contains("visibility: internal"), "{stdout}");

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("policy.json");
    std::fs::write(&file, r#"{"visibility":"team"}"#).unwrap();
    let (code, _, stderr) = cli(
        &["policy", "set", "r/a", "-f", file.to_str().unwrap()],
        &s.url(),
        &[],
    );
    assert_eq!(code, 0, "{stderr}");

    let (code, _, _) = cli(&["policy", "delete", "r/a"], &s.url(), &[]);
    assert_eq!(code, 9, "a policy delete confirms");
    let (code, _, stderr) = cli(&["policy", "delete", "r/a", "-y"], &s.url(), &[]);
    assert_eq!(code, 0, "{stderr}");

    show.assert();
    set.assert();
    delete.assert();
}

#[test]
fn exemption_add_list_remove_and_policy_set_gate_are_one_endpoint() {
    let mut s = server();
    let path = "/api/v1/admin/registries/r/policy/version/a/1.0.0/rules/cve_gate";
    let body = json!({"exempt_until": "2027-01-01T00:00:00Z", "reason": "not reachable"});
    let put = s
        .mock("PUT", path)
        .match_body(Matcher::Json(body))
        .with_body(
            r#"{"gate":"cve_gate","exempt_until":"2027-01-01T00:00:00Z",
                "reason":"not reachable","self_approved":true}"#,
        )
        .expect(2)
        .create();
    let remove = s.mock("DELETE", path).with_status(204).create();
    let list = s
        .mock("GET", "/api/v1/admin/registries/r/exemptions")
        .with_body(
            r#"[{"package":"a","version":"1.0.0","gate":"cve_gate","exempt_until":"2027-01-01T00:00:00Z",
                 "reason":"not reachable","self_approved":true,"expired":false}]"#,
        )
        .create();

    let gate = [
        "--gate",
        "cve_gate",
        "--until",
        "2027-01-01T00:00:00Z",
        "--reason",
        "not reachable",
    ];
    let (code, stdout, stderr) = cli(
        &[&["exemption", "add", "r/a@1.0.0"][..], &gate].concat(),
        &s.url(),
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("Exempted r/a@1.0.0 from cve_gate"),
        "{stdout}"
    );
    assert!(stderr.contains("self-approved"), "{stderr}");
    let (code, _, stderr) = cli(
        &[&["policy", "set", "r/a@1.0.0"][..], &gate].concat(),
        &s.url(),
        &[],
    );
    assert_eq!(code, 0, "{stderr}");

    let (code, stdout, _) = cli(&["exemption", "list", "-r", "r"], &s.url(), &[]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("GATE") && stdout.contains("cve_gate"),
        "{stdout}"
    );
    let (code, _, _) = cli(&["exemption", "list"], &s.url(), &[]);
    assert_eq!(code, 2, "no registry is a usage error");

    let (code, _, stderr) = cli(
        &["exemption", "remove", "r/a@1.0.0", "--gate", "cve_gate"],
        &s.url(),
        &[],
    );
    assert_eq!(code, 0, "{stderr}");

    put.assert();
    remove.assert();
    list.assert();
}

// ── server config pending · apply · discard · warnings ───────────────────────

#[test]
fn server_config_pending_apply_discard_and_warnings() {
    let mut s = server();
    let diff = json!({"added_registries": ["new"], "removed_registries": [], "changed_registries": [],
        "applied": [], "partially_applied": [], "restart_required": [],
        "access_config_changed": true, "limits_changed": false, "restart_reason": null});
    let pending = s
        .mock("GET", "/api/v1/admin/config/pending")
        .with_body(
            json!({"id": "p1", "created_at": "t0", "expires_at": "t1", "source": "file_watcher",
                "diff": diff, "warnings": []})
            .to_string(),
        )
        .create();
    let apply = s
        .mock("POST", "/api/v1/admin/config/pending/apply")
        .with_body(json!({"diff": diff, "warnings": [], "pending_created": false}).to_string())
        .expect(1)
        .create();
    let discard = s
        .mock("DELETE", "/api/v1/admin/config/pending")
        .with_status(204)
        .expect(1)
        .create();
    let warnings = s
        .mock("GET", "/api/v1/admin/config/warnings")
        .with_body(r#"{"warnings":[{"code":"W1","path":"registries.x","message":"unused"}]}"#)
        .create();

    let (code, stdout, _) = cli(&["server", "config", "pending"], &s.url(), &[]);
    assert_eq!(code, 0);
    assert!(stdout.contains("added_registries: new"), "{stdout}");
    assert!(stdout.contains("access_config_changed: yes"), "{stdout}");
    assert!(!stdout.contains("removed_registries"), "{stdout}");

    for verb in ["apply", "discard"] {
        let (code, _, _) = cli(&["server", "config", verb], &s.url(), &[]);
        assert_eq!(code, 9, "{verb} confirms");
        let (code, _, stderr) = cli(&["server", "config", verb, "-y"], &s.url(), &[]);
        assert_eq!(code, 0, "{verb}: {stderr}");
    }

    let (code, stdout, _) = cli(
        &["server", "config", "warnings", "--format", "json"],
        &s.url(),
        &[],
    );
    assert_eq!(code, 0);
    assert_eq!(json_of(&stdout)["warnings"][0]["code"], "W1");

    pending.assert();
    apply.assert();
    discard.assert();
    warnings.assert();
}

// ── flag create · delete ─────────────────────────────────────────────────────

fn hub_signature(secret: &str, data: &[u8]) -> String {
    use hmac::{Hmac, KeyInit, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(data);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

#[test]
fn flag_create_signs_the_body_and_flags_every_version_without_one() {
    let mut s = server();
    // The signature must be over the exact bytes sent: the mock recomputes it.
    let push = s
        .mock("POST", "/api/v1/flags/soc")
        .match_request(|req| {
            let body = req.body().unwrap();
            let sent = req.header("X-Hub-Signature-256");
            let v: Value = serde_json::from_slice(body).unwrap();
            let flag = &v["flags"][0];
            sent.len() == 1
                && sent[0].to_str().unwrap() == hub_signature("s3cret", body)
                && flag["version_range"] == "*"
                && flag.get("version").is_none()
                && flag["package_name"] == "evil"
                && flag["effect"] == "hard_block"
        })
        .with_body(
            r#"{"accepted":1,"rejected":0,"items":[{"status":"accepted","external_id":"X-1",
                "id":"00000000-0000-0000-0000-000000000000","created":true,
                "effect":"hard_block","effect_capped":false}]}"#,
        )
        .create();
    let (code, stdout, stderr) = cli(
        &[
            "flag",
            "create",
            "r/evil",
            "--source",
            "soc",
            "--external-id",
            "X-1",
            "--effect",
            "hard_block",
            "--summary",
            "typosquat",
        ],
        &s.url(),
        &[("BATLEHUB_FLAG_SECRET", "s3cret")],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("Raised X-1 on r/evil: hard_block"),
        "{stdout}"
    );
    push.assert();
}

#[test]
fn flag_create_reports_a_rejected_item_as_a_failure() {
    let mut s = server();
    s.mock("POST", "/api/v1/flags/soc")
        .with_body(
            r#"{"accepted":0,"rejected":1,"items":[{"status":"rejected","external_id":"X-1",
                "error":"unknown registry"}]}"#,
        )
        .create();
    let (code, _, stderr) = cli(
        &[
            "flag",
            "create",
            "r/evil@1.0.0",
            "--source",
            "soc",
            "--external-id",
            "X-1",
            "--effect",
            "warn",
            "--summary",
            "s",
        ],
        &s.url(),
        &[("BATLEHUB_FLAG_SECRET", "s3cret")],
    );
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("rejected: unknown registry"), "{stderr}");
}

#[test]
fn flag_delete_confirms_and_signs_the_canonical_request() {
    let mut s = server();
    let canonical = "DELETE\n/api/v1/flags/soc/X-1";
    let revoke = s
        .mock("DELETE", "/api/v1/flags/soc/X-1")
        .match_header(
            "X-Hub-Signature-256",
            hub_signature("s3cret", canonical.as_bytes()).as_str(),
        )
        .with_body(r#"{"revoked":true}"#)
        .expect(1)
        .create();
    let args = ["flag", "delete", "X-1", "--source", "soc"];
    let env = [("BATLEHUB_FLAG_SECRET", "s3cret")];
    let (code, _, _) = cli(&args, &s.url(), &env);
    assert_eq!(code, 9);
    let (code, stdout, stderr) = cli(&[&args[..], &["-y"]].concat(), &s.url(), &env);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("Revoked X-1"), "{stdout}");
    revoke.assert();
}

// ── me ───────────────────────────────────────────────────────────────────────

#[test]
fn me_show_quota_downloads_advisories_namespaces() {
    let mut s = server();
    s.mock("GET", "/api/v1/me")
        .with_body(
            r#"{"user_id":"alice","role":"user","groups":["eng"],"has_registry_access":true}"#,
        )
        .create();
    s.mock("GET", "/api/v1/me/quota")
        .with_body(r#"[{"registry":"r","bytes_used":10,"packages_used":1,"warn_threshold_pct":80,"state":"ok"}]"#)
        .create();
    let denied = s
        .mock("GET", "/api/v1/me/downloads")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("outcome".into(), "denied".into()),
            Matcher::UrlEncoded("limit".into(), "5".into()),
        ]))
        .with_body(
            r#"[{"registry":"r","name":"a","version":"1.0.0","downloaded_at":"t",
                 "blocked":true,"reason_code":"ADMIN_BLOCK"}]"#,
        )
        .create();
    s.mock("GET", "/api/v1/me/advisories")
        .with_body(r#"{"advisories":[],"window_days":30,"scanning_available":false}"#)
        .create();
    s.mock("GET", "/api/v1/me/namespaces")
        .with_body(r#"[{"registry":"r","prefix":"@eng/","group_id":"eng","package_count":3}]"#)
        .create();

    let (code, stdout, _) = cli(&["me", "show"], &s.url(), &[]);
    assert_eq!(code, 0);
    assert!(stdout.contains("user_id: alice"), "{stdout}");

    let (code, stdout, _) = cli(&["me", "quota", "--format", "csv"], &s.url(), &[]);
    assert_eq!(code, 0);
    assert!(
        stdout.starts_with("bytes_used,packages_used,registry,state"),
        "{stdout}"
    );

    let (code, stdout, stderr) = cli(
        &["me", "downloads", "--denied", "--limit", "5"],
        &s.url(),
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("REASON_CODE") && stdout.contains("ADMIN_BLOCK"),
        "{stdout}"
    );
    denied.assert();

    let (code, stdout, stderr) = cli(&["me", "advisories"], &s.url(), &[]);
    assert_eq!(code, 0);
    assert!(stdout.contains("(none)"), "{stdout}");
    assert!(stderr.contains("no scanner"), "{stderr}");

    let (code, stdout, _) = cli(&["me", "namespaces"], &s.url(), &[]);
    assert_eq!(code, 0);
    assert!(stdout.contains("@eng/"), "{stdout}");
}

// ── upstream ─────────────────────────────────────────────────────────────────

#[test]
fn upstream_status_recheck_and_disappeared() {
    let mut s = server();
    let row = json!({"registry": "r", "package_name": "a", "version": "1.0.0", "state": "disappeared",
        "consecutive_misses": 3, "first_missed_at": "t0", "last_checked_at": "t1"});
    s.mock("GET", "/api/v1/admin/upstream/status/r/a")
        .with_body(
            json!({"registry": "r", "package_name": "a", "policy": "audit", "rows": [row]})
                .to_string(),
        )
        .create();
    let recheck = s
        .mock("POST", "/api/v1/admin/upstream/recheck")
        .match_body(Matcher::Json(json!({"registry": "r", "package_name": "a", "version": "1.0.0"})))
        .with_body(
            r#"{"registry":"r","package_name":"a","probed":1,"missing":1,"inconclusive":0,"transitions":[]}"#,
        )
        .create();
    let page = s
        .mock("GET", "/api/v1/admin/upstream/disappeared")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("registry".into(), "r".into()),
            Matcher::UrlEncoded("state".into(), "disappeared".into()),
        ]))
        .with_body(
            json!({"items": [row], "total": 1, "page": 0, "per_page": 50, "policy": "audit",
                "registries": ["r"], "counts": []})
            .to_string(),
        )
        .create();

    let (code, stdout, _) = cli(&["upstream", "status", "r/a"], &s.url(), &[]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("STATE") && stdout.contains("disappeared"),
        "{stdout}"
    );

    let (code, stdout, stderr) = cli(&["upstream", "recheck", "r/a@1.0.0"], &s.url(), &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("probed 1, missing 1"), "{stdout}");
    recheck.assert();

    let (code, stdout, stderr) = cli(
        &[
            "upstream",
            "disappeared",
            "-r",
            "r",
            "--state",
            "disappeared",
            "--format",
            "csv",
        ],
        &s.url(),
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("disappeared"), "{stdout}");
    page.assert();
}

// ── namespace signing-key ────────────────────────────────────────────────────

#[test]
fn namespace_signing_key_list_set_delete() {
    let mut s = server();
    let base = "/api/v1/admin/registries/r/signing-keys/hashicorp";
    s.mock("GET", base)
        .with_body(r#"{"keys":[{"key_id":"ABCD","ascii_armor":"…","source":"manual"}]}"#)
        .create();
    let set = s
        .mock("PUT", base)
        .match_body(Matcher::PartialJson(
            json!({"key_id": "ABCD", "ascii_armor": "-----BEGIN-----\n"}),
        ))
        .with_body(r#"{"ok":true}"#)
        .create();
    let delete = s
        .mock("DELETE", format!("{base}/ABCD").as_str())
        .with_body(r#"{"ok":true}"#)
        .expect(1)
        .create();

    let (code, stdout, _) = cli(
        &["namespace", "signing-key", "list", "hashicorp", "-r", "r"],
        &s.url(),
        &[],
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains("KEY_ID") && stdout.contains("ABCD"),
        "{stdout}"
    );

    let dir = tempfile::tempdir().unwrap();
    let armor = dir.path().join("key.asc");
    std::fs::write(&armor, "-----BEGIN-----\n").unwrap();
    let (code, _, stderr) = cli(
        &[
            "namespace",
            "signing-key",
            "set",
            "hashicorp",
            "-r",
            "r",
            "--key-id",
            "ABCD",
            "--armor",
            armor.to_str().unwrap(),
        ],
        &s.url(),
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    set.assert();

    let del = [
        "namespace",
        "signing-key",
        "delete",
        "hashicorp",
        "ABCD",
        "-r",
        "r",
    ];
    let (code, _, _) = cli(&del, &s.url(), &[]);
    assert_eq!(code, 9);
    let (code, _, stderr) = cli(&[&del[..], &["-y"]].concat(), &s.url(), &[]);
    assert_eq!(code, 0, "{stderr}");
    delete.assert();
}
