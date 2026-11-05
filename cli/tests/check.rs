//! `batlehub-cli check` (RFC 0040 §4.8, phase 7) against a stand-in server.
//!
//! One `<kind>_check` row per ✅ cell of the coverage matrix: a lockfile in
//! the shape its tool writes, pinning one dependency, and the coordinate the
//! batch must be asked about. The server's own answer is `pullability.rs`'s to
//! prove; this proves the CLI reads the file, picks the registry of the kind
//! and asks for the right `registry/name@version`.

use mockito::Matcher;
use serde_json::{json, Value};

fn run(dir: &std::path::Path, args: &[&str], server: &str) -> (i32, String, String) {
    let home = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_batlehub-cli"))
        .args(args)
        .current_dir(dir)
        .env("BATLEHUB_SERVER", server)
        .env("BATLEHUB_TOKEN", "t")
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env_remove("BATLEHUB_REGISTRY")
        .env_remove("BATLEHUB_FORMAT")
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn answer(headline: &str, causes: Value) -> Value {
    json!({"headline": headline, "can_access": headline == "pullable", "causes": causes})
}

/// A stand-in listing one proxy registry `r` of `kind`, whose batch expects
/// exactly `items` and answers each with `answers`.
fn stand_in(kind: &str, items: Value, answers: Value) -> (mockito::ServerGuard, mockito::Mock) {
    let mut s = mockito::Server::new();
    s.mock("GET", "/api/v1/registries")
        .with_body(json!([{"name": "r", "type": kind, "mode": "proxy"}]).to_string())
        .create();
    let batch = s
        .mock("POST", "/api/v1/packages/access/batch")
        .match_body(Matcher::Json(json!({ "items": items })))
        .with_body(json!({ "items": answers }).to_string())
        .expect(1)
        .create();
    (s, batch)
}

fn one_pin(kind: &str, file: &str, content: &str, name: &str, version: &str) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(file), content).unwrap();
    let (s, batch) = stand_in(
        kind,
        json!([{"registry": "r", "name": name, "version": version}]),
        json!([answer("pullable", json!([]))]),
    );
    let (code, stdout, stderr) = run(dir.path(), &["check", "--format", "json"], &s.url());
    assert_eq!(code, 0, "{kind}: {stderr}");
    let rows: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        rows[0]["coordinate"],
        format!("r/{name}@{version}"),
        "{kind}"
    );
    assert_eq!(rows[0]["state"], "pullable");
    assert_eq!(rows[0]["lockfile"], file);
    batch.assert();
}

macro_rules! check_case {
    ($($test:ident, $kind:literal, $file:literal, $content:expr, $name:literal, $version:literal;)*) => {
        $(
            #[test]
            fn $test() {
                one_pin($kind, $file, $content, $name, $version);
            }
        )*
    };
}

check_case! {
    cargo_check, "cargo", "Cargo.lock",
        "[[package]]\nname = \"serde\"\nversion = \"1.0.200\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
        "serde", "1.0.200";
    npm_check, "npm", "package-lock.json",
        r#"{"lockfileVersion":3,"packages":{"":{"name":"app"},"node_modules/left-pad":{"version":"1.3.0"}}}"#,
        "left-pad", "1.3.0";
    pypi_check, "pypi", "uv.lock",
        "version = 1\n[[package]]\nname = \"requests\"\nversion = \"2.32.3\"\nsource = { registry = \"https://pypi.org/simple\" }\n",
        "requests", "2.32.3";
    goproxy_check, "goproxy", "go.sum",
        "golang.org/x/mod v0.20.0 h1:a=\ngolang.org/x/mod v0.20.0/go.mod h1:b=\n",
        "golang.org/x/mod", "v0.20.0";
    conda_check, "conda", "conda-lock.yml",
        "version: 1\npackage:\n  - name: numpy\n    version: 1.26.4\n    manager: conda\n",
        "numpy", "1.26.4";
    composer_check, "composer", "composer.lock",
        r#"{"packages":[{"name":"monolog/monolog","version":"3.7.0"}]}"#,
        "monolog/monolog", "3.7.0";
    rubygems_check, "rubygems", "Gemfile.lock",
        "GEM\n  remote: https://rubygems.org/\n  specs:\n    rack (3.1.7)\n",
        "rack", "3.1.7";
    nuget_check, "nuget", "packages.lock.json",
        r#"{"version":1,"dependencies":{"net8.0":{"Newtonsoft.Json":{"type":"Direct","resolved":"13.0.3"}}}}"#,
        "Newtonsoft.Json", "13.0.3";
    terraform_check, "terraform", ".terraform.lock.hcl",
        "provider \"registry.terraform.io/hashicorp/aws\" {\n  version = \"5.60.0\"\n}\n",
        "providers/hashicorp/aws", "5.60.0";
    helm_check, "helm", "Chart.lock",
        "dependencies:\n- name: redis\n  repository: https://r\n  version: 19.6.0\n",
        "redis", "19.6.0";
    nodedist_check, "nodedist", ".nvmrc", "v20.11.0\n", "node", "v20.11.0";
    sdkman_check, "sdkman", ".sdkmanrc", "java=21.0.2-tem\n", "java", "21.0.2-tem";
    rustup_check, "rustup", "rust-toolchain.toml",
        "[toolchain]\nchannel = \"1.80.0\"\n", "rust", "1.80.0";
    github_check, "github", "mise.lock", &mise_lock("https://github.com/BurntSushi/ripgrep/releases/download/14.1.0/rg.tar.gz"),
        "BurntSushi/ripgrep", "14.1.0";
    forgejo_check, "forgejo", "mise.lock", &mise_lock("https://codeberg.org/forgejo/forgejo/releases/download/v9.0.0/forgejo.xz"),
        "forgejo/forgejo", "v9.0.0";
}

/// A `mise.lock` with one tool downloaded from `url`, on every platform the
/// test may run on.
fn mise_lock(url: &str) -> String {
    let mut out = String::from("[[tools.tool]]\nversion = \"1\"\nbackend = \"aqua:x/y\"\n");
    for platform in [
        "linux-x64",
        "linux-arm64",
        "macos-x64",
        "macos-arm64",
        "windows-x64",
    ] {
        out.push_str(&format!(
            "[tools.tool.\"platforms.{platform}\"]\nurl = \"{url}\"\n"
        ));
    }
    out
}

const LOCK: &str = r#"{"lockfileVersion":3,"packages":{"node_modules/a":{"version":"1.0.0"},"node_modules/b":{"version":"2.0.0"}}}"#;

fn two(answers: Value) -> (tempfile::TempDir, mockito::ServerGuard, mockito::Mock) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package-lock.json"), LOCK).unwrap();
    let (s, batch) = stand_in(
        "npm",
        json!([
            {"registry": "r", "name": "a", "version": "1.0.0"},
            {"registry": "r", "name": "b", "version": "2.0.0"},
        ]),
        answers,
    );
    (dir, s, batch)
}

/// §4.6: ⏳ passes, and fails with `--strict`; the table names the dependency,
/// its reason and the next step.
#[test]
fn check_lists_what_waits_and_strict_fails_on_it() {
    let wait = answer(
        "waiting",
        json!([{"kind": "verdict", "state": "wait", "reason_code": "MIN_AGE_NOT_MET",
                "detail": "released today", "next": "batlehub-cli wait r/b@2.0.0"}]),
    );
    let (dir, s, _) = two(json!([answer("pullable", json!([])), wait]));
    let (code, stdout, stderr) = run(dir.path(), &["check"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("r/b@2.0.0") && stdout.contains("MIN_AGE_NOT_MET"),
        "{stdout}"
    );
    assert!(stdout.contains("batlehub-cli wait r/b@2.0.0"), "{stdout}");
    assert!(
        !stdout.contains("r/a@1.0.0"),
        "a pullable one is not listed: {stdout}"
    );
    assert!(
        stderr.contains("2 dependencies: 1 pullable, 1 waiting, 0 refused"),
        "{stderr}"
    );
    let (code, _, _) = run(dir.path(), &["check", "--strict"], &s.url());
    assert_eq!(code, 4);
}

/// ⛔ and an unknown or invisible coordinate both fail the check, and the
/// JSON is one document.
#[test]
fn check_fails_on_a_refusal_or_a_not_found() {
    let refuse = answer(
        "refused",
        json!([{"kind": "block", "state": "refuse", "reason_code": "BLOCK_LIST", "detail": "blocked"}]),
    );
    let (dir, s, _) = two(json!([refuse, {"state": "not_found"}]));
    let (code, stdout, _) = run(dir.path(), &["check", "--format", "json"], &s.url());
    assert_eq!(code, 4);
    let rows: Value = serde_json::from_str(&stdout).expect("one JSON document");
    assert_eq!(rows[0]["reason_code"], "BLOCK_LIST");
    assert_eq!(rows[1]["state"], "not_found");
}

/// T9: a lockfile over 16 MiB is refused with exit 2, before any request.
#[test]
fn check_rejects_oversized_lockfile() {
    let dir = tempfile::tempdir().unwrap();
    let big = std::fs::File::create(dir.path().join("go.sum")).unwrap();
    big.set_len(17 * 1024 * 1024).unwrap();
    let mut s = mockito::Server::new();
    let any = s.mock("GET", Matcher::Any).expect(0).create();
    let (code, _, stderr) = run(dir.path(), &["check", "--lock", "go.sum"], &s.url());
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("over the"), "{stderr}");
    any.assert();
}

/// 120 pins are three batches of 50, 50 and 20, one at a time.
#[test]
fn check_asks_fifty_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let lines: String = (0..120)
        .map(|i| format!("example.com/m{i:03} v1.0.0 h1:x=\n"))
        .collect();
    std::fs::write(dir.path().join("go.sum"), lines).unwrap();
    let mut s = mockito::Server::new();
    s.mock("GET", "/api/v1/registries")
        .with_body(r#"[{"name":"g","type":"goproxy","mode":"proxy"}]"#)
        .create();
    let mut mocks = Vec::new();
    for (first, n) in [(0, 50), (50, 50), (100, 20)] {
        let items: Vec<Value> = (first..first + n)
            .map(|i| json!({"registry": "g", "name": format!("example.com/m{i:03}"), "version": "v1.0.0"}))
            .collect();
        let answers: Vec<Value> = (0..n).map(|_| answer("pullable", json!([]))).collect();
        mocks.push(
            s.mock("POST", "/api/v1/packages/access/batch")
                .match_body(Matcher::Json(json!({ "items": items })))
                .with_body(json!({ "items": answers }).to_string())
                .expect(1)
                .create(),
        );
    }
    let (code, _, stderr) = run(dir.path(), &["check"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("120 dependencies: 120 pullable"),
        "{stderr}"
    );
    for m in mocks {
        m.assert();
    }
}

/// A lockfile whose kind no registry here serves is said, not silently
/// passed; with nothing left to ask, nothing is sent.
#[test]
fn check_says_when_no_registry_serves_a_lockfile() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Gemfile.lock"),
        "GEM\n  specs:\n    rack (3.1.7)\n",
    )
    .unwrap();
    let mut s = mockito::Server::new();
    s.mock("GET", "/api/v1/registries")
        .with_body(r#"[{"name":"r","type":"npm","mode":"proxy"}]"#)
        .create();
    let batch = s
        .mock("POST", "/api/v1/packages/access/batch")
        .expect(0)
        .create();
    let (code, _, stderr) = run(dir.path(), &["check"], &s.url());
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("no rubygems registry"), "{stderr}");
    batch.assert();
}

#[test]
fn check_with_no_lockfile_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let (code, _, stderr) = run(dir.path(), &["check"], "http://127.0.0.1:9");
    assert_eq!(code, 2, "{stderr}");
}
