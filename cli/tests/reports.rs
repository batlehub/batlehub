//! The renderers of `verdict`, `wait`, `audit pulls`, the admin reports and
//! `mise seed|export|import|plan` (RFC 0041 §6.6).
//!
//! The in-memory server of `integration.rs` has no security profile, no flag
//! source, no bundle store and no audit seal, so every one of these commands
//! would assert its error path there. Each test here runs the binary against a
//! stand-in that answers what the API answers, and asserts on what the command
//! prints and the code it exits with — the same contract `journeys.rs` holds.

use mockito::Matcher;
use serde_json::{json, Value};

const TOKEN: &str = "reports-token";

fn cli(args: &[&str], server: &str) -> (i32, String, String) {
    let home = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_batlehub-cli"))
        .args(args)
        .env("BATLEHUB_SERVER", server)
        .env("BATLEHUB_TOKEN", TOKEN)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env_remove("BATLEHUB_REGISTRY")
        .env_remove("BATLEHUB_FORMAT")
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
        .with_body(r#"[{"name":"r","type":"npm","mode":"proxy"}]"#)
        .create();
    s
}

/// `path` with or without a query string.
fn at(path: &str) -> Matcher {
    Matcher::Regex(format!(r"^{}(\?.*)?$", path.replace('.', r"\.")))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

// ── verdict · audit pulls · wait ─────────────────────────────────────────────

fn verdict(state: &str, codes: &[&str], available_at: Option<&str>) -> String {
    json!({
        "package": {"registry": "r", "name": "a", "version": "1.0.0"},
        "state": state,
        "reason_codes": codes,
        "policy_ref": "default",
        "available_at": available_at,
        "evaluated_at": "2026-11-01T00:00:00Z",
    })
    .to_string()
}

#[test]
fn verdict_list_prints_one_row_per_verdict_with_its_codes() {
    let mut s = server();
    s.mock("GET", at("/api/v1/admin/verdicts"))
        .with_body(
            json!({"items": [{
                "package": {"name": "left-pad", "version": "1.3.0"},
                "state": "quarantined",
                "last_scanned_at": "2026-10-30T12:00:00Z",
                "reason_codes": ["RELEASE_AGE", "SCAN_PENDING"],
            }]})
            .to_string(),
        )
        .create();
    let (code, stdout, stderr) = cli(&["verdict", "list", "-r", "r"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("1 verdict(s) in r")
            && stdout.contains("left-pad@1.3.0")
            && stdout.contains("2026-10-30 ")
            && stdout.contains("RELEASE_AGE,SCAN_PENDING"),
        "{stdout}"
    );
}

#[test]
fn verdict_list_of_an_empty_registry_prints_no_header() {
    let mut s = server();
    s.mock("GET", at("/api/v1/admin/verdicts"))
        .with_body(r#"{"items":[]}"#)
        .create();
    let (code, stdout, stderr) = cli(&["verdict", "list", "-r", "r"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout.trim(), "0 verdict(s) in r");
}

#[test]
fn verdict_list_json_prints_the_api_answer() {
    let mut s = server();
    s.mock("GET", at("/api/v1/admin/verdicts"))
        .with_body(r#"{"items":[],"total":0}"#)
        .create();
    let (code, stdout, stderr) = cli(
        &["verdict", "list", "-r", "r", "--format", "json"],
        &s.url(),
    );
    assert_eq!(code, 0, "{stderr}");
    let v: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["total"], 0);
}

#[test]
fn verdict_pullers_counts_one_identity_in_the_singular() {
    let mut s = server();
    s.mock("GET", at("/api/v1/verdicts/r/a/1.0.0/pullers"))
        .with_body(
            json!({"since": "2026-10-01T00:00:00Z", "pullers": [{
                "identity": "alice", "role": "user", "count": 3,
                "first_pull": "2026-10-02T00:00:00Z", "last_pull": "2026-10-03T00:00:00Z",
            }]})
            .to_string(),
        )
        .create();
    let (code, stdout, stderr) = cli(&["verdict", "pullers", "r/a@1.0.0"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("r/a@1.0.0 pulled by 1 identity since 2026-10-01T00:00:00Z")
            && stdout.contains("alice"),
        "{stdout}"
    );
}

#[test]
fn verdict_pullers_json_prints_the_server_body_verbatim() {
    let mut s = server();
    let body = r#"{"since":"x","pullers":[]}"#;
    s.mock("GET", at("/api/v1/verdicts/r/a/1.0.0/pullers"))
        .with_body(body)
        .create();
    let (code, stdout, stderr) = cli(
        &["verdict", "pullers", "r/a@1.0.0", "--format", "json"],
        &s.url(),
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, format!("{body}\n"));
}

#[test]
fn verdict_pullers_of_nobody_says_zero_identities() {
    let mut s = server();
    s.mock("GET", at("/api/v1/verdicts/r/a/1.0.0/pullers"))
        .with_body(r#"{"since":"2026-10-01T00:00:00Z","pullers":[]}"#)
        .create();
    let (_, stdout, _) = cli(&["verdict", "pullers", "r/a@1.0.0"], &s.url());
    assert_eq!(
        stdout.trim(),
        "r/a@1.0.0 pulled by 0 identities since 2026-10-01T00:00:00Z"
    );
}

#[test]
fn audit_pulls_prints_what_one_identity_pulled() {
    let mut s = server();
    s.mock("GET", at("/api/v1/audit/pulls"))
        .match_query(Matcher::UrlEncoded("identity".into(), "alice".into()))
        .with_body(
            json!({"since": "2026-10-01T00:00:00Z", "pulls": [
                {"registry": "r", "package_name": "left-pad", "version": "1.3.0",
                 "count": 2, "last_pull": "2026-10-03T00:00:00Z"},
                {"registry": "r", "package_name": "lodash", "count": 1,
                 "last_pull": "2026-10-04T00:00:00Z"},
            ]})
            .to_string(),
        )
        .create();
    let (code, stdout, stderr) = cli(&["audit", "pulls", "--identity", "alice"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("alice pulled 2 coordinates since 2026-10-01T00:00:00Z")
            && stdout.contains("left-pad")
            && stdout.contains("lodash"),
        "{stdout}"
    );
}

#[test]
fn audit_pulls_json_prints_the_server_body_verbatim() {
    let mut s = server();
    s.mock("GET", at("/api/v1/audit/pulls"))
        .with_body("{\"since\":\"x\",\"pulls\":[]}\n")
        .create();
    let (code, stdout, stderr) = cli(
        &[
            "audit",
            "pulls",
            "--identity",
            "ip:10.0.0.1",
            "--package",
            "left-pad",
            "-r",
            "r",
            "--format",
            "json",
        ],
        &s.url(),
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "{\"since\":\"x\",\"pulls\":[]}\n");
}

#[test]
fn audit_pulls_of_nothing_prints_no_header() {
    let mut s = server();
    s.mock("GET", at("/api/v1/audit/pulls"))
        .with_body(r#"{"since":"s","pulls":[{"registry":"r","package_name":"p","count":1,"last_pull":"t"}]}"#)
        .create();
    let (_, stdout, _) = cli(&["audit", "pulls", "--identity", "bob"], &s.url());
    assert!(
        stdout.starts_with("bob pulled 1 coordinate since s"),
        "{stdout}"
    );
}

#[test]
fn verdict_backfill_reports_what_was_queued() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/verdicts/backfill")
        .match_body(Matcher::Json(json!({"registry": "r", "state": null})))
        .with_body(r#"{"queued":3,"considered":5,"trigger":"backfill"}"#)
        .create();
    let (code, stdout, stderr) = cli(&["verdict", "backfill", "-r", "r"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stdout.trim(),
        "backfill: 3 of 5 coordinate(s) queued at backfill priority in r"
    );
}

#[test]
fn verdict_rescan_json_prints_the_api_answer() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/verdicts/rescan")
        .match_body(Matcher::Json(json!({"registry": "r", "state": "denied"})))
        .with_body(r#"{"queued":1,"considered":1,"trigger":"rescan"}"#)
        .create();
    let (code, stdout, stderr) = cli(
        &[
            "verdict", "rescan", "-r", "r", "--state", "denied", "--format", "json",
        ],
        &s.url(),
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(serde_json::from_str::<Value>(&stdout).unwrap()["queued"], 1);
}

#[test]
fn wait_exits_zero_on_a_served_verdict() {
    let mut s = server();
    s.mock("GET", "/api/v1/verdicts/r/a/1.0.0")
        .with_body(verdict("allowed", &[], None))
        .create();
    let (code, stdout, stderr) = cli(&["wait", "r/a@1.0.0"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout.trim(), "r/a@1.0.0 is allowed (no findings)");
}

#[test]
fn wait_names_the_codes_of_a_warned_verdict() {
    let mut s = server();
    s.mock("GET", "/api/v1/verdicts/r/a/1.0.0")
        .with_body(verdict("warned", &["CVE_LOW"], None))
        .create();
    let (_, stdout, _) = cli(&["wait", "r/a@1.0.0"], &s.url());
    assert_eq!(stdout.trim(), "r/a@1.0.0 is warned (CVE_LOW)");
}

#[test]
fn wait_json_prints_the_verdict() {
    let mut s = server();
    s.mock("GET", "/api/v1/verdicts/r/a/1.0.0")
        .with_body(verdict("allowed", &[], None))
        .create();
    let (_, stdout, _) = cli(&["wait", "r/a@1.0.0", "--format", "json"], &s.url());
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap()["state"],
        "allowed"
    );
}

#[test]
fn wait_exits_four_at_once_on_a_denied_verdict() {
    let mut s = server();
    s.mock("GET", "/api/v1/verdicts/r/a/1.0.0")
        .with_body(verdict("denied", &["MALWARE"], None))
        .expect(1)
        .create();
    let (code, _, stderr) = cli(&["wait", "r/a@1.0.0"], &s.url());
    assert_eq!(code, 4, "{stderr}");
    assert!(
        stderr.contains("waiting cannot help — denied (MALWARE)"),
        "{stderr}"
    );
}

#[test]
fn wait_exits_four_when_the_instance_has_no_verdict() {
    let mut s = server();
    s.mock("GET", "/api/v1/verdicts/r/a/1.0.0")
        .with_status(404)
        .create();
    let (code, _, stderr) = cli(&["wait", "r/a@1.0.0"], &s.url());
    assert_eq!(code, 4, "{stderr}");
    assert!(stderr.contains("no verdict for r/a@1.0.0"), "{stderr}");
}

#[test]
fn wait_exits_eight_when_the_hold_outlasts_the_timeout() {
    let mut s = server();
    s.mock("GET", "/api/v1/verdicts/r/a/1.0.0")
        .with_body(verdict("quarantined", &["SCAN_PENDING"], None))
        .create();
    let (code, _, stderr) = cli(
        &["wait", "r/a@1.0.0", "--timeout", "1s", "--interval", "1s"],
        &s.url(),
    );
    assert_eq!(code, 8, "{stderr}");
    assert!(
        stderr.contains("still quarantined (SCAN_PENDING)"),
        "{stderr}"
    );
}

#[test]
fn wait_sleeps_to_a_clock_rather_than_the_interval() {
    let mut s = server();
    s.mock("GET", "/api/v1/verdicts/r/a/1.0.0")
        .with_body(verdict(
            "quarantined",
            &["RELEASE_AGE"],
            Some("2999-01-01T00:00:00Z"),
        ))
        .create();
    let started = std::time::Instant::now();
    let (code, _, stderr) = cli(
        &["wait", "r/a@1.0.0", "--timeout", "1s", "--interval", "1h"],
        &s.url(),
    );
    assert_eq!(code, 8, "{stderr}");
    // A far clock is capped by the timeout, never slept to.
    assert!(started.elapsed() < std::time::Duration::from_secs(30));
}

// ── exposure · flag · bundle · air-gap ───────────────────────────────────────

fn exposure(rows: Value, next: Option<&str>, last_scan: Value) -> String {
    json!({
        "rows": rows,
        "next": next,
        "coverage": {
            "registries_total": 1, "sbom_configured": 1, "security_profiles": 0,
            "last_scan": last_scan,
            "flag_sources": [
                {"source": "osv", "live_flags": 2, "last_push_at": "2026-10-05T10:00:00Z"},
                {"source": "intel", "live_flags": 0},
            ],
        },
    })
    .to_string()
}

#[test]
fn exposure_list_renders_rows_the_cursor_and_the_coverage() {
    let mut s = server();
    s.mock("GET", at("/api/v1/admin/exposure"))
        .with_body(exposure(
            json!([{
                "consumer": "alice", "consumer_role": "user", "registry": "r",
                "package_name": "left-pad", "version": "1.3.0", "source": "osv",
                "external_id": "GHSA-1", "kind": "vulnerability", "effect": "gate",
                "summary": "bad", "flag_first_seen": "2026-10-01T00:00:00Z",
                "pulls": 4, "pulls_before_flag": 1,
                "first_pull": "2026-09-01T00:00:00Z", "last_pull": "2026-10-02T08:30:00Z",
            }]),
            Some("cursor-2"),
            json!([{"registry": "r", "last_scan_at": "2026-10-06T00:00:00Z",
                    "artifacts_scanned": 9, "findings": 1, "errors": 0}]),
        ))
        .create();
    let (code, stdout, stderr) = cli(&["exposure", "list"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    for want in [
        "osv:GHSA-1",
        "1 row(s)",
        "more follow: --after cursor-2",
        "coverage: 1 registry, 1 with an SBOM extractor, 0 with a security profile",
        "r: scanned 9 at 2026-10-06 00:00 (1 finding(s), 0 error(s))",
        "source osv: 2 live flag(s), last push 2026-10-05 10:00",
        "source intel: 0 live flag(s), last push never",
    ] {
        assert!(stdout.contains(want), "missing {want:?}: {stdout}");
    }
}

#[test]
fn exposure_list_without_a_scan_says_the_report_knows_only_pushes() {
    let mut s = server();
    s.mock("GET", at("/api/v1/admin/exposure"))
        .with_body(exposure(json!([]), None, json!([])))
        .create();
    let (_, stdout, _) = cli(&["exposure", "list", "--when", "before-flag"], &s.url());
    assert!(
        stdout.contains("CVE scan: never recorded a pass"),
        "{stdout}"
    );
}

#[test]
fn exposure_list_json_prints_the_api_answer() {
    let mut s = server();
    s.mock("GET", at("/api/v1/admin/exposure"))
        .with_body(exposure(json!([]), None, json!([])))
        .create();
    let (_, stdout, _) = cli(&["exposure", "list", "--format", "json"], &s.url());
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap()["coverage"]["registries_total"],
        1
    );
}

#[test]
fn flag_list_shows_revoked_expiring_and_live_states() {
    let flag = |id: &str, expires: Value, revoked: Value| {
        json!({
            "id": "00000000-0000-0000-0000-000000000001", "source": "osv",
            "external_id": id, "registry": "r", "package_name": "a", "version": "1.0.0",
            "kind": "malware", "effect": "hard_block", "summary": "s",
            "first_seen": "2026-10-01T00:00:00Z", "updated_at": "2026-10-01T00:00:00Z",
            "expires_at": expires, "revoked_at": revoked,
        })
    };
    let mut s = server();
    s.mock("GET", at("/api/v1/admin/flags"))
        .with_body(
            json!({"items": [
                flag("F-1", Value::Null, json!("2026-10-02T00:00:00Z")),
                flag("F-2", json!("2026-12-31T00:00:00Z"), Value::Null),
                flag("F-3", Value::Null, Value::Null),
            ], "total": 3, "page": 0, "per_page": 50})
            .to_string(),
        )
        .create();
    let (code, stdout, stderr) = cli(&["flag", "list"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("revoked")
            && stdout.contains("expires 2026-12-31")
            && stdout.contains("live")
            && stdout.contains("3 of 3 flag(s), page 0"),
        "{stdout}"
    );
}

#[test]
fn flag_list_json_prints_the_api_answer() {
    let mut s = server();
    s.mock("GET", at("/api/v1/admin/flags"))
        .with_body(r#"{"items":[],"total":0,"page":0,"per_page":50}"#)
        .create();
    let (_, stdout, _) = cli(&["flag", "list", "--format", "json"], &s.url());
    assert_eq!(serde_json::from_str::<Value>(&stdout).unwrap()["total"], 0);
}

#[test]
fn bundle_list_prints_the_signer_prefix_and_the_count() {
    let mut s = server();
    s.mock("GET", "/api/v1/admin/bundle")
        .with_body(
            json!({"items": [{
                "bundle_id": "estate-1", "signer_key": "abcdef0123456789",
                "imported_at": "2026-10-01T09:00:00Z", "imported_by": "ops",
                "entries": 3, "blobs": 2, "rejected": 0,
            }]})
            .to_string(),
        )
        .create();
    let (code, stdout, stderr) = cli(&["bundle", "list"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("abcdef01")
            && !stdout.contains("abcdef0123")
            && stdout.contains("1 bundle(s)"),
        "{stdout}"
    );
}

#[test]
fn bundle_list_empty_explains_what_nothing_imported_means() {
    let mut s = server();
    s.mock("GET", "/api/v1/admin/bundle")
        .with_body(r#"{"items":[]}"#)
        .create();
    let (_, stdout, _) = cli(&["bundle", "list"], &s.url());
    assert!(stdout.contains("nothing has been imported"), "{stdout}");
}

#[test]
fn bundle_list_json_prints_the_items() {
    let mut s = server();
    s.mock("GET", "/api/v1/admin/bundle")
        .with_body(r#"{"items":[]}"#)
        .create();
    let (_, stdout, _) = cli(&["bundle", "list", "--format", "json"], &s.url());
    assert_eq!(serde_json::from_str::<Value>(&stdout).unwrap(), json!([]));
}

#[test]
fn air_gap_missing_caps_held_versions_and_warns_when_not_air_gapped() {
    let miss = |requested: Value, held: Value| {
        json!({
            "registry": "r", "storage_key": "r/a/1.2.0", "kind": "artifact",
            "first_seen": "2026-10-01T00:00:00Z", "last_seen": "2026-10-02T00:00:00Z",
            "count": 2, "requested_version": requested, "held_versions": held,
        })
    };
    let mut s = server();
    s.mock("GET", at("/api/v1/admin/air-gap/missing"))
        .with_body(
            json!({"items": [
                miss(json!("1.2.0"), json!(["1", "2", "3", "4", "5", "6"])),
                miss(Value::Null, json!([])),
            ], "total": 2, "page": 0, "per_page": 100, "air_gapped": false})
            .to_string(),
        )
        .create();
    let (code, stdout, stderr) = cli(&["air-gap", "missing"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("1, 2, 3, 4 (+2)")
            && stdout.contains("2 of 2 row(s)")
            && stdout.contains("this instance is not air-gapped"),
        "{stdout}"
    );
}

#[test]
fn air_gap_missing_json_prints_the_api_answer() {
    let mut s = server();
    s.mock("GET", at("/api/v1/admin/air-gap/missing"))
        .with_body(r#"{"items":[],"total":0,"page":0,"per_page":100,"air_gapped":true}"#)
        .create();
    let (_, stdout, _) = cli(&["air-gap", "missing", "--format", "json"], &s.url());
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap()["air_gapped"],
        true
    );
}

// ── gdpr · audit verify ──────────────────────────────────────────────────────

#[test]
fn gdpr_erase_reports_the_pseudonym_and_the_counts() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/gdpr/erase")
        .match_body(Matcher::Json(json!({"user_id": "alice", "force": false})))
        .with_body(r#"{"pseudonym":"subj-1","audit_rows":4,"tokens":1,"blocks":0}"#)
        .create();
    let (code, stdout, stderr) = cli(&["gdpr", "erase", "--user", "alice", "-y"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stdout.trim(),
        "Erased: now subj-1 — 4 audit row(s), 1 token(s) revoked, 0 block row(s)"
    );
}

#[test]
fn gdpr_erase_json_prints_the_api_answer() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/gdpr/erase")
        .with_body(r#"{"pseudonym":"subj-1"}"#)
        .create();
    let (_, stdout, _) = cli(
        &["gdpr", "erase", "--user", "alice", "-y", "--format", "json"],
        &s.url(),
    );
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap()["pseudonym"],
        "subj-1"
    );
}

#[test]
fn gdpr_export_prints_the_subject_document() {
    let mut s = server();
    s.mock("GET", at("/api/v1/admin/gdpr/export"))
        .match_query(Matcher::UrlEncoded("user_id".into(), "alice".into()))
        .with_body(r#"{"subject":"alice","tokens":[]}"#)
        .create();
    let (code, stdout, stderr) = cli(&["gdpr", "export", "--user", "alice"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap()["subject"],
        "alice"
    );
}

#[test]
fn audit_verify_lists_each_failed_window_and_fails() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/audit/verify")
        .with_body(
            json!({"ok": false, "records": 10, "windows_checked": 2, "windows_expired": 0,
                   "head": "h1", "truncation_checked": true,
                   "failures": [{"window_start": "t0", "window_end": "t1", "seq": 7,
                                 "reason": "digest mismatch"}]})
            .to_string(),
        )
        .create();
    let (code, stdout, stderr) = cli(&["audit", "verify", "--head", "h1"], &s.url());
    assert_eq!(code, 1, "{stdout}{stderr}");
    assert!(
        stdout.contains("FAIL t0 … t1 (record 7): digest mismatch"),
        "{stdout}"
    );
}

#[test]
fn audit_verify_without_a_head_warns_that_truncation_was_not_checked() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/audit/verify")
        .with_body(
            r#"{"ok":true,"records":3,"windows_checked":1,"windows_expired":1,
                "truncation_checked":false,"failures":[]}"#,
        )
        .create();
    let (code, stdout, stderr) = cli(&["audit", "verify"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("3 record(s), 1 window(s) checked, 1 expired by retention")
            && stdout.contains("truncation NOT checked")
            && stdout.trim_end().ends_with("ok"),
        "{stdout}"
    );
}

#[test]
fn audit_verify_json_still_exits_one_on_a_failure() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/audit/verify")
        .with_body(r#"{"ok":false,"records":0,"failures":[]}"#)
        .create();
    let (code, stdout, _) = cli(&["audit", "verify", "--format", "json"], &s.url());
    assert_eq!(code, 1, "{stdout}");
}

// ── server config ────────────────────────────────────────────────────────────

fn reload(pending_created: bool) -> String {
    json!({
        "diff": {
            "added_registries": [], "removed_registries": [], "changed_registries": [],
            "access_config_changed": false, "limits_changed": false,
            "applied": ["registries.r.upstream"],
            "partially_applied": ["limits.rate"],
            "restart_required": ["server.port"],
            "restart_reason": "the listener is bound once",
        },
        "pending_created": pending_created,
    })
    .to_string()
}

fn toml_file(dir: &tempfile::TempDir) -> String {
    let p = dir.path().join("config.toml");
    std::fs::write(&p, "[server]\nport = 9000\n").unwrap();
    p.to_str().unwrap().to_owned()
}

#[test]
fn server_config_show_prints_the_content() {
    let mut s = server();
    s.mock("GET", "/api/v1/admin/config/content")
        .with_body(r#"{"content":"[server]\nport = 8080","is_readonly":false}"#)
        .create();
    let (code, stdout, stderr) = cli(&["server", "config", "show"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout.trim(), "[server]\nport = 8080");
}

#[test]
fn server_config_validate_says_what_needs_a_restart_and_why() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/config/validate")
        .match_body(Matcher::Json(json!({"content": "[server]\nport = 9000\n"})))
        .with_body(reload(false))
        .create();
    let dir = tempfile::tempdir().unwrap();
    let file = toml_file(&dir);
    let (code, stdout, stderr) = cli(&["server", "config", "validate", &file], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains(": valid")
            && stdout.contains("takes effect: registries.r.upstream")
            && stdout.contains("needs a restart: server.port")
            && stdout.contains("(the listener is bound once)"),
        "{stdout}"
    );
}

#[test]
fn server_config_validate_of_a_missing_file_names_it() {
    let s = server();
    let (code, _, stderr) = cli(
        &["server", "config", "validate", "/nonexistent/batlehub.toml"],
        &s.url(),
    );
    assert_ne!(code, 0);
    assert!(
        stderr.contains("could not read /nonexistent/batlehub.toml"),
        "{stderr}"
    );
}

#[test]
fn server_config_from_file_says_it_was_staged_not_applied() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/config/from-content")
        .with_body(reload(true))
        .create();
    let dir = tempfile::tempdir().unwrap();
    let file = toml_file(&dir);
    let (code, stdout, stderr) = cli(&["server", "config", "from-file", &file], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("apply it within 10 minutes"), "{stdout}");
}

#[test]
fn server_config_from_file_unchanged_stages_nothing() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/config/from-content")
        .with_body(reload(false))
        .create();
    let dir = tempfile::tempdir().unwrap();
    let file = toml_file(&dir);
    let (_, stdout, _) = cli(&["server", "config", "from-file", &file], &s.url());
    assert!(
        stdout.contains("unchanged since the last load, nothing staged"),
        "{stdout}"
    );
}

#[test]
fn server_config_validate_json_prints_the_diff() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/config/validate")
        .with_body(reload(false))
        .create();
    let dir = tempfile::tempdir().unwrap();
    let file = toml_file(&dir);
    let (_, stdout, _) = cli(
        &["server", "config", "validate", &file, "--format", "json"],
        &s.url(),
    );
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap()["diff"]["restart_required"][0],
        "server.port"
    );
}

#[test]
fn server_config_changes_prints_each_change() {
    let mut s = server();
    s.mock("GET", "/api/v1/admin/config/changes")
        .with_body(
            r#"{"items":[{"id":"1","triggered_by":"alice","triggered_at":"2026-10-01",
                "summary":"added r","status":"applied"}]}"#,
        )
        .create();
    let (code, stdout, stderr) = cli(&["server", "config", "changes"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("alice") && stdout.contains("added r"),
        "{stdout}"
    );
}

#[test]
fn server_config_show_json_prints_the_content_object() {
    let mut s = server();
    s.mock("GET", "/api/v1/admin/config/content")
        .with_body(r#"{"content":"x","is_readonly":true}"#)
        .create();
    let (_, stdout, _) = cli(&["server", "config", "show", "--format", "json"], &s.url());
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap()["is_readonly"],
        true
    );
}

// ── retention · import · cache ───────────────────────────────────────────────

#[test]
fn retention_show_lists_reclaim_kept_and_every_footnote() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/registries/r/retention?dry_run=true")
        .with_body(
            json!({"registry": "r", "examined": 5, "reclaimed": 1, "kept": 4, "dry_run": true,
                   "reclaimed_coordinates": ["a@0.1.0"],
                   "decisions": [{"name": "a", "version": "1.0.0", "kept_because": "latest"},
                                 {"name": "a", "version": "0.1.0"}],
                   "decisions_truncated": 2,
                   "incomplete_because": "time budget spent"})
            .to_string(),
        )
        .create();
    let (code, stdout, stderr) = cli(&["retention", "show", "r", "--show-kept"], &s.url());
    assert_eq!(code, 0, "{stdout}{stderr}");
    for want in [
        "Retention on r: dry run — nothing was changed",
        "examined 5   kept 4   reclaimed 1",
        "would reclaim:\n  a@0.1.0",
        "kept:\n  a@1.0.0  (latest)",
        "(2 more per-version decisions not shown",
        "Nothing was deleted. To reclaim",
        "RUN INCOMPLETE: time budget spent",
    ] {
        assert!(stdout.contains(want), "missing {want:?}: {stdout}");
    }
}

#[test]
fn retention_apply_reclaims_after_the_confirmation() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/registries/r/retention?dry_run=false")
        .with_body(
            r#"{"registry":"r","examined":2,"reclaimed":1,"kept":1,"dry_run":false,
                "reclaimed_coordinates":["a@0.1.0"]}"#,
        )
        .create();
    let (code, stdout, stderr) = cli(&["retention", "apply", "r", "-y"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("LIVE — versions were reclaimed")
            && stdout.contains("reclaimed:\n  a@0.1.0"),
        "{stdout}"
    );
}

#[test]
fn retention_show_json_prints_the_report() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/registries/r/retention?dry_run=true")
        .with_body(r#"{"registry":"r","examined":0,"reclaimed":0,"kept":0,"dry_run":true}"#)
        .create();
    let (_, stdout, _) = cli(&["retention", "show", "r", "--format", "json"], &s.url());
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap()["dry_run"],
        true
    );
}

#[test]
fn import_lists_each_failure_by_tag_and_asset() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/registries/r/import")
        .with_body(
            r#"{"imported":1,"skipped":2,"errors":1,
                "failures":[{"tag":"v1.2.0","asset":"tool.tar.gz","error":"404 upstream"}]}"#,
        )
        .create();
    let (code, stdout, stderr) = cli(&["import", "r"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("Release import on r: imported 1, skipped 2, errors 1")
            && stdout.contains("tool.tar.gz")
            && stdout.contains("404 upstream"),
        "{stdout}"
    );
}

#[test]
fn import_json_prints_the_report() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/registries/r/import")
        .with_body(r#"{"imported":0,"skipped":0,"errors":0}"#)
        .create();
    let (_, stdout, _) = cli(&["import", "r", "--format", "json"], &s.url());
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap()["imported"],
        0
    );
}

#[test]
fn cache_evict_dry_run_lists_keys_the_sample_and_why_it_stopped() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/registries/r/evict?dry_run=true")
        .with_body(
            r#"{"total":2,"evicted_ttl":1,"evicted_idle":0,"evicted_old_versions":1,
                "evicted_lru":0,"dry_run":true,"evicted_keys":["r/a/1.0.0"],
                "keys_truncated":1,"incomplete_because":"storage listing failed"}"#,
        )
        .create();
    let (code, stdout, stderr) = cli(&["cache", "evict", "--dry-run", "-r", "r"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("would evict:\n  r/a/1.0.0")
            && stdout.contains("… and 1 more not listed")
            && stdout.contains("storage listing failed"),
        "{stdout}"
    );
}

#[test]
fn cache_coherence_defers_a_first_seen_orphan_to_the_next_sweep() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/registries/r/coherence?dry_run=false")
        .with_body(
            r#"{"storage_keys":3,"meta_rows":1,"orphaned_deleted":1,"deleted_keys":["r/x"],
                "first_seen_orphaned":1,"first_seen_keys":["r/y"],"keys_truncated":4,
                "dry_run":false}"#,
        )
        .create();
    let (code, stdout, stderr) = cli(&["cache", "coherence", "-r", "r", "-y"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("deleted 1\n    r/x")
            && stdout.contains("1 newly orphaned, deferred to the next sweep:\n    r/y")
            && stdout.contains("… and 4 more not listed"),
        "{stdout}"
    );
}

// ── notification · sbom ──────────────────────────────────────────────────────

#[test]
fn notification_channels_lists_each_channel() {
    let mut s = server();
    s.mock("GET", "/api/v1/admin/notifications/channels")
        .with_body(r#"[{"name":"ops-slack"}]"#)
        .create();
    let (code, stdout, stderr) = cli(&["notification", "channels"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("ops-slack"), "{stdout}");
}

#[test]
fn notification_list_shows_a_wildcard_for_an_unscoped_subscription() {
    let mut s = server();
    s.mock("GET", "/api/v1/admin/notifications/subscriptions")
        .with_body(
            r#"[{"id":"00000000-0000-0000-0000-000000000009","registry":null,
                 "package_name":null,"event_types":["package_published","package_yanked"],
                 "channel_name":"ops-slack","created_by":"alice",
                 "created_at":"2026-10-01T00:00:00Z","enabled":true}]"#,
        )
        .create();
    let (code, stdout, stderr) = cli(&["notification", "list"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("package_published,package_yanked") && stdout.contains("*"),
        "{stdout}"
    );
}

#[test]
fn notification_list_json_prints_the_subscriptions() {
    let mut s = server();
    s.mock("GET", "/api/v1/admin/notifications/subscriptions")
        .with_body("[]")
        .create();
    let (_, stdout, _) = cli(&["notification", "list", "--format", "json"], &s.url());
    assert_eq!(serde_json::from_str::<Value>(&stdout).unwrap(), json!([]));
}

#[test]
fn notification_channels_json_prints_the_channels() {
    let mut s = server();
    s.mock("GET", "/api/v1/admin/notifications/channels")
        .with_body(r#"[{"name":"c"}]"#)
        .create();
    let (_, stdout, _) = cli(&["notification", "channels", "--format", "json"], &s.url());
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap()[0]["name"],
        "c"
    );
}

#[test]
fn notification_delete_sends_the_delete_after_the_confirmation() {
    let mut s = server();
    let del = s
        .mock("DELETE", "/api/v1/admin/notifications/subscriptions/sub-1")
        .with_status(204)
        .expect(1)
        .create();
    let (code, stdout, stderr) = cli(&["notification", "delete", "sub-1", "-y"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout.trim(), "Deleted subscription sub-1");
    del.assert();
}

#[test]
fn sbom_export_writes_the_document_to_the_named_file() {
    let mut s = server();
    s.mock("GET", at("/api/v1/sbom/export"))
        .with_body(r#"{"bomFormat":"CycloneDX"}"#)
        .create();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("sbom.json");
    let (code, stdout, stderr) = cli(
        &["sbom", "export", "-r", "r", "-o", out.to_str().unwrap()],
        &s.url(),
    );
    assert_eq!(code, 0, "{stdout}{stderr}");
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(written["bomFormat"], "CycloneDX");
}

#[test]
fn sbom_export_prints_to_stdout_without_a_file() {
    let mut s = server();
    s.mock("GET", at("/api/v1/sbom/export"))
        .with_body(r#"{"bomFormat":"CycloneDX"}"#)
        .create();
    let (code, stdout, stderr) = cli(&["sbom", "export"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap()["bomFormat"],
        "CycloneDX"
    );
}

// ── mise seed · export · import · plan ───────────────────────────────────────

fn write_plan(dir: &tempfile::TempDir, plan_version: u32, entries: Value) -> String {
    let plan = json!({
        "plan_version": plan_version,
        "generated_from": {"file": "mise.lock", "sha256": "f".repeat(64)},
        "platforms": ["linux-x64"],
        "entries": entries,
        "unsupported": [],
        "unmirrored_hosts": [],
    });
    let p = dir.path().join("plan.json");
    std::fs::write(&p, plan.to_string()).unwrap();
    p.to_str().unwrap().to_owned()
}

fn entry(tool: &str, reg: Option<(&str, &str)>, path: Option<&str>, sha: Option<&str>) -> Value {
    let mut e = json!({
        "tool": tool, "version": "1.0.0", "platform": "linux-x64",
        "url": format!("https://example.invalid/{tool}"),
    });
    if let Some((name, kind)) = reg {
        e["registry"] = json!({"name": name, "type": kind});
        e["key"] = json!(format!("{name}/{tool}/1.0.0"));
    }
    if let Some(p) = path {
        e["proxy_path"] = json!(p);
    }
    if let Some(h) = sha {
        e["sha256"] = json!(h);
    }
    e
}

fn seed_server() -> (mockito::ServerGuard, String) {
    let mut s = server();
    s.mock("GET", "/proxy/g/ok")
        .with_body("good bytes")
        .create();
    s.mock("GET", "/proxy/g/drift")
        .with_body("other bytes")
        .create();
    s.mock("GET", "/proxy/g/held")
        .with_status(403)
        .with_header("X-BatleHub-Verdict", "denied")
        .with_header("X-BatleHub-Reason", "MALWARE")
        .with_body("denied")
        .create();
    s.mock("GET", "/proxy/g/gone").with_status(404).create();
    s.mock("GET", "/proxy/g/nosum")
        .with_header("X-BatleHub-Verdict", "warned")
        .with_header("X-BatleHub-Reason", "PROVENANCE_MISSING")
        .with_body("x")
        .create();
    (s, sha256_hex(b"good bytes"))
}

fn seed_entries(good: &str) -> Value {
    let g = Some(("g", "generic"));
    json!([
        entry("ok", g, Some("/proxy/g/ok"), Some(good)),
        entry("drift", g, Some("/proxy/g/drift"), Some(good)),
        entry("held", g, Some("/proxy/g/held"), None),
        entry("gone", g, Some("/proxy/g/gone"), None),
        entry("nosum", g, Some("/proxy/g/nosum"), None),
        entry("loose", None, None, None),
    ])
}

#[test]
fn mise_seed_reports_each_failed_entry_and_exits_seven() {
    let (s, good) = seed_server();
    let dir = tempfile::tempdir().unwrap();
    let plan = write_plan(&dir, 1, seed_entries(&good));
    let (code, stdout, stderr) = cli(&["mise", "seed", "--plan", &plan, "--verify"], &s.url());
    assert_eq!(code, 7, "{stdout}{stderr}");
    for want in [
        "2/6 fetched and verified",
        "digest_mismatch [linux-x64] drift",
        "denied [linux-x64] held",
        "verdict denied (MALWARE)",
        "failed [linux-x64] gone",
        "HTTP 404",
        "unmirrored [linux-x64] loose",
        "1 denied · 1 with no provenance",
    ] {
        assert!(stdout.contains(want), "missing {want:?}: {stdout}");
    }
}

#[test]
fn mise_seed_json_counts_the_seeded_entries() {
    let (s, good) = seed_server();
    let dir = tempfile::tempdir().unwrap();
    let plan = write_plan(&dir, 1, seed_entries(&good));
    let (code, stdout, _) = cli(
        &["mise", "seed", "--plan", &plan, "--format", "json"],
        &s.url(),
    );
    assert_eq!(code, 7);
    // Under --format json a partial failure follows the report with its own
    // error document, so the report is the first of two.
    let v: Value = serde_json::Deserializer::from_str(&stdout)
        .into_iter()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(
        (v["seeded"].as_u64(), v["total"].as_u64()),
        (Some(2), Some(6))
    );
}

#[test]
fn mise_seed_refuses_a_plan_newer_than_the_cli() {
    let s = server();
    let dir = tempfile::tempdir().unwrap();
    let plan = write_plan(&dir, 99, json!([]));
    let (code, _, stderr) = cli(&["mise", "seed", "--plan", &plan], &s.url());
    assert_ne!(code, 0);
    assert!(stderr.contains("is plan_version 99"), "{stderr}");
}

#[test]
fn mise_seed_of_a_file_that_is_not_a_plan_says_so() {
    let s = server();
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("plan.json");
    std::fs::write(&p, "not json").unwrap();
    let (code, _, stderr) = cli(&["mise", "seed", "--plan", p.to_str().unwrap()], &s.url());
    assert_ne!(code, 0);
    assert!(stderr.contains("is not a plan"), "{stderr}");
}

fn bundle_file(dir: &tempfile::TempDir) -> String {
    let p = dir.path().join("estate.bhub");
    std::fs::write(&p, b"bundle").unwrap();
    p.to_str().unwrap().to_owned()
}

#[test]
fn mise_import_lists_each_rejection_and_exits_seven() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/bundle/import")
        .with_body(
            r#"{"bundle_id":"b","signer_key":"0123456789abcdef","imported":2,"entries":3,
                "rejected":1,"rejections":["r/x: digest mismatch"]}"#,
        )
        .create();
    let dir = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = cli(&["mise", "import", &bundle_file(&dir)], &s.url());
    assert_eq!(code, 7, "{stderr}");
    assert!(
        stdout.contains("signature ok (01234567) · 2 blob(s) · 1 rejected")
            && stdout.contains("rejected: r/x: digest mismatch"),
        "{stdout}"
    );
}

#[test]
fn mise_import_of_an_imported_bundle_writes_nothing_again() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/bundle/import")
        .with_body(
            r#"{"bundle_id":"b","signer_key":"k","imported":0,"entries":3,"rejected":0,
                "already_imported":true}"#,
        )
        .create();
    let dir = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = cli(&["mise", "import", &bundle_file(&dir)], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("bundle b was already imported"), "{stdout}");
}

#[test]
fn mise_import_json_with_a_rejection_still_exits_seven() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/bundle/import")
        .with_body(r#"{"bundle_id":"b","signer_key":"k","imported":0,"entries":1,"rejected":1}"#)
        .create();
    let dir = tempfile::tempdir().unwrap();
    let (code, stdout, _) = cli(
        &["mise", "import", &bundle_file(&dir), "--format", "json"],
        &s.url(),
    );
    assert_eq!(code, 7, "{stdout}");
}

#[test]
fn mise_import_json_of_a_clean_bundle_exits_zero() {
    let mut s = server();
    s.mock("POST", "/api/v1/admin/bundle/import")
        .with_body(r#"{"bundle_id":"b","signer_key":"k","imported":1,"entries":1,"rejected":0}"#)
        .create();
    let dir = tempfile::tempdir().unwrap();
    let (code, stdout, _) = cli(
        &["mise", "import", &bundle_file(&dir), "--format", "json"],
        &s.url(),
    );
    assert_eq!(code, 0, "{stdout}");
}

#[test]
fn mise_import_of_a_missing_file_names_it() {
    let s = server();
    let (code, _, stderr) = cli(&["mise", "import", "/nonexistent/x.bhub"], &s.url());
    assert_ne!(code, 0);
    assert!(stderr.contains("reading /nonexistent/x.bhub"), "{stderr}");
}

const TF: &str = "/proxy/tf/v1/providers/hashicorp/null";

/// A stand-in for an export: one entry carried with every header, one whose
/// bytes disagree with the lock, one that fails, one with no mirror, and two
/// Terraform provider archives — one whose download document answers.
fn export_server() -> (mockito::ServerGuard, Value) {
    let mut s = server();
    s.mock("GET", "/proxy/g/ok")
        .with_header("X-BatleHub-Storage-Key", "artifact:g/stored/ok")
        .with_header("X-BatleHub-Package", "owner/repo")
        .with_header("X-BatleHub-Version", "1.0.0")
        .with_header("X-BatleHub-Ref-Requested", "v1")
        .with_header("X-BatleHub-Ref-Kind", "tag")
        .with_header("X-BatleHub-Resolved-Commit", "abc123")
        .with_body("good bytes")
        .create();
    s.mock("GET", "/api/v1/verdicts/g/owner%2Frepo/1.0.0")
        .with_body(
            json!({"package": {"registry": "g", "name": "owner/repo", "version": "1.0.0"},
                   "state": "allowed", "policy_ref": "p", "evaluated_at": "t"})
            .to_string(),
        )
        .create();
    s.mock("GET", "/proxy/g/drift").with_body("other").create();
    s.mock("GET", "/proxy/g/gone").with_status(404).create();
    s.mock("GET", format!("{TF}/3.2.2/artifact/linux/amd64").as_str())
        .with_body("provider")
        .create();
    s.mock("GET", format!("{TF}/3.2.2/download/linux/amd64").as_str())
        .with_body(r#"{"protocols":["5.0"],"signing_keys":{"gpg_public_keys":[]}}"#)
        .create();
    s.mock("GET", format!("{TF}/3.2.1/artifact/linux/amd64").as_str())
        .with_body("older provider")
        .create();
    let good = sha256_hex(b"good bytes");
    let g = Some(("g", "github"));
    let tf = Some(("tf", "terraform"));
    let entries = json!([
        entry("ok", g, Some("/proxy/g/ok"), Some(&good)),
        entry("drift", g, Some("/proxy/g/drift"), Some(&good)),
        entry("gone", g, Some("/proxy/g/gone"), None),
        entry("loose", None, None, None),
        entry(
            "tf-new",
            tf,
            Some(&format!("{TF}/3.2.2/artifact/linux/amd64")),
            None
        ),
        entry(
            "tf-old",
            tf,
            Some(&format!("{TF}/3.2.1/artifact/linux/amd64")),
            None
        ),
    ]);
    (s, entries)
}

fn key_file(dir: &tempfile::TempDir, content: &str) -> String {
    let p = dir.path().join("estate.key");
    std::fs::write(&p, content).unwrap();
    p.to_str().unwrap().to_owned()
}

#[test]
fn mise_export_says_why_each_entry_was_skipped() {
    let (s, entries) = export_server();
    let dir = tempfile::tempdir().unwrap();
    let plan = write_plan(&dir, 1, entries);
    let key = key_file(&dir, &("0".repeat(63) + "1"));
    let out = dir.path().join("estate.bhub");
    let (code, stdout, stderr) = cli(
        &[
            "mise",
            "export",
            "--plan",
            &plan,
            "--sign-key",
            &key,
            "-o",
            out.to_str().unwrap(),
        ],
        &s.url(),
    );
    assert_eq!(code, 0, "{stdout}{stderr}");
    for want in [
        "3 entries · 3 blob(s) · signed",
        "skipped: drift [linux-x64]: the lock says",
        "skipped: loose [linux-x64]: no mirror",
        "skipped: tf-new [linux-x64]: the server did not report a storage key",
        "skipped: tf-old [linux-x64]: the provider's download document",
        "the plan names the provider archive but not",
        "[air_gap].bundle_trusted_keys",
    ] {
        assert!(stdout.contains(want), "missing {want:?}: {stdout}");
    }
    assert!(stdout.contains("skipped: gone [linux-x64]: "), "{stdout}");
}

#[test]
fn mise_export_json_reports_the_public_half_of_the_key() {
    let (s, entries) = export_server();
    let dir = tempfile::tempdir().unwrap();
    let plan = write_plan(&dir, 1, entries);
    let key = key_file(&dir, &("0".repeat(63) + "1"));
    let out = dir.path().join("estate.bhub");
    let (code, stdout, stderr) = cli(
        &[
            "mise",
            "export",
            "--plan",
            &plan,
            "--sign-key",
            &key,
            "-o",
            out.to_str().unwrap(),
            "--format",
            "json",
        ],
        &s.url(),
    );
    assert_eq!(code, 0, "{stderr}");
    let v: Value = serde_json::from_str(&stdout).unwrap();
    // The plan's digest names the bundle when no --bundle-id is given.
    assert_eq!(
        (
            v["bundle_id"].as_str(),
            v["signer_key"].as_str().map(str::len)
        ),
        (Some("f".repeat(64).as_str()), Some(64))
    );
}

#[test]
fn mise_export_refuses_a_key_that_is_not_hex() {
    let s = server();
    let dir = tempfile::tempdir().unwrap();
    let plan = write_plan(&dir, 1, json!([]));
    let key = key_file(&dir, "not-hex");
    let (code, _, stderr) = cli(
        &["mise", "export", "--plan", &plan, "--sign-key", &key],
        &s.url(),
    );
    assert_ne!(code, 0);
    assert!(stderr.contains("is not hex"), "{stderr}");
}

#[test]
fn mise_export_refuses_a_key_of_the_wrong_length() {
    let s = server();
    let dir = tempfile::tempdir().unwrap();
    let plan = write_plan(&dir, 1, json!([]));
    let key = key_file(&dir, "abcd");
    let (code, _, stderr) = cli(
        &["mise", "export", "--plan", &plan, "--sign-key", &key],
        &s.url(),
    );
    assert_ne!(code, 0);
    assert!(
        stderr.contains("is 2 bytes; an ed25519 signing key is 32"),
        "{stderr}"
    );
}

const PLAN_LOCK: &str = r#"
[[tools."aqua:EmbarkStudios/cargo-deny"]]
version = "0.18.2"
backend = "aqua:EmbarkStudios/cargo-deny"

[tools."aqua:EmbarkStudios/cargo-deny"."platforms.PLATFORM"]
url = "https://github.com/EmbarkStudios/cargo-deny/releases/download/0.18.2/cargo-deny.tar.gz"

[[tools.sonar]]
version = "5.0"
[tools.sonar."platforms.PLATFORM"]
url = "https://binaries.sonarsource.com/sonar-scanner-5.0.zip"
"#;

fn write_lock(dir: &tempfile::TempDir, platform: &str) -> String {
    let p = dir.path().join("mise.lock");
    std::fs::write(&p, PLAN_LOCK.replace("PLATFORM", platform)).unwrap();
    p.to_str().unwrap().to_owned()
}

fn plan_server() -> mockito::ServerGuard {
    let mut s = mockito::Server::new();
    s.mock("GET", "/api/v1/registries")
        .with_body(r#"[{"name":"gh","type":"github","mode":"proxy"}]"#)
        .create();
    s
}

#[test]
fn mise_plan_written_to_a_file_prints_the_one_line_summary() {
    let s = plan_server();
    let dir = tempfile::tempdir().unwrap();
    let lock = write_lock(&dir, "linux-x64");
    let out = dir.path().join("plan.json");
    let (code, stdout, stderr) = cli(
        &[
            "mise",
            "plan",
            "--lock",
            &lock,
            "--platform",
            "linux-x64",
            "-o",
            out.to_str().unwrap(),
        ],
        &s.url(),
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("2 tool(s) · 2 download(s) · 1 registry · 1 host(s) with no mirror")
            && stdout.contains("no mirror: binaries.sonarsource.com")
            && stdout.contains("plan written to"),
        "{stdout}"
    );
    assert!(out.exists());
}

#[test]
fn mise_plan_json_with_a_file_prints_nothing() {
    let s = plan_server();
    let dir = tempfile::tempdir().unwrap();
    let lock = write_lock(&dir, "linux-x64");
    let out = dir.path().join("plan.json");
    let (code, stdout, stderr) = cli(
        &[
            "mise",
            "plan",
            "--lock",
            &lock,
            "--platform",
            "linux-x64",
            "-o",
            out.to_str().unwrap(),
            "--format",
            "json",
        ],
        &s.url(),
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "");
}

#[test]
fn mise_plan_for_a_host_the_lock_does_not_record_plans_every_platform() {
    let s = plan_server();
    let dir = tempfile::tempdir().unwrap();
    // No host this suite runs on is a riscv64 FreeBSD.
    let lock = write_lock(&dir, "freebsd-riscv64");
    let (code, stdout, stderr) = cli(&["mise", "plan", "--lock", &lock], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("records no entry for this host"),
        "{stderr}"
    );
    let plan: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(plan["entries"].as_array().map(Vec::len), Some(2));
}
