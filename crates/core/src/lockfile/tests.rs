//! One fixture per format, in the shape its tool writes, with the entries a
//! reader must skip beside the ones it must keep.

use super::*;

fn pins(format: Format, text: &str) -> Vec<(String, String)> {
    read(format, text.as_bytes())
        .unwrap()
        .into_iter()
        .map(|p| {
            assert_eq!(p.kind, format.kind());
            (p.name, p.version)
        })
        .collect()
}

fn pair(n: &str, v: &str) -> (String, String) {
    (n.to_owned(), v.to_owned())
}

#[test]
fn cargo_lock_keeps_registry_packages() {
    let lock = r#"
version = 4
[[package]]
name = "app"
version = "0.1.0"
[[package]]
name = "serde"
version = "1.0.200"
source = "registry+https://github.com/rust-lang/crates.io-index"
[[package]]
name = "tokio"
version = "1.40.0"
source = "sparse+https://hub.example/proxy/cargo/index/"
[[package]]
name = "forked"
version = "0.2.0"
source = "git+https://github.com/x/forked#abc"
"#;
    assert_eq!(
        pins(Format::CargoLock, lock),
        vec![pair("serde", "1.0.200"), pair("tokio", "1.40.0")]
    );
}

#[test]
fn package_lock_v3_reads_nested_and_scoped_and_skips_links() {
    let lock = r#"{
  "lockfileVersion": 3,
  "packages": {
    "": {"name": "app", "version": "1.0.0"},
    "node_modules/left-pad": {"version": "1.3.0", "resolved": "https://r/left-pad/-/left-pad-1.3.0.tgz"},
    "node_modules/@scope/pkg": {"version": "2.0.0"},
    "node_modules/a/node_modules/b": {"version": "0.1.0"},
    "node_modules/local": {"link": true, "resolved": "packages/local"},
    "node_modules/fromfile": {"version": "1.0.0", "resolved": "file:../x.tgz"}
  }
}"#;
    assert_eq!(
        pins(Format::PackageLock, lock),
        vec![
            pair("@scope/pkg", "2.0.0"),
            pair("b", "0.1.0"),
            pair("left-pad", "1.3.0")
        ]
    );
}

#[test]
fn package_lock_v1_walks_dependencies() {
    let lock = r#"{"lockfileVersion": 1, "dependencies": {
        "a": {"version": "1.0.0", "dependencies": {"b": {"version": "2.0.0"}}},
        "c": {"version": "github:x/c#abc"}}}"#;
    assert_eq!(
        pins(Format::PackageLock, lock),
        vec![pair("a", "1.0.0"), pair("b", "2.0.0")]
    );
}

#[test]
fn pnpm_lock_reads_v9_v6_and_v5_keys() {
    let lock = r#"
lockfileVersion: '9.0'
packages:
  left-pad@1.3.0:
    resolution: {integrity: sha512-x}
  '@scope/pkg@2.0.0':
    resolution: {integrity: sha512-y}
  /old@6.0.0(react@18.0.0):
    resolution: {integrity: sha512-z}
  /older/5.0.0:
    resolution: {integrity: sha512-w}
  local@link:../local:
    resolution: {directory: ../local}
"#;
    assert_eq!(
        pins(Format::PnpmLock, lock),
        vec![
            pair("@scope/pkg", "2.0.0"),
            pair("left-pad", "1.3.0"),
            pair("old", "6.0.0"),
            pair("older", "5.0.0"),
        ]
    );
}

#[test]
fn yarn_lock_reads_v1_and_berry_and_skips_workspaces() {
    let v1 = "# yarn lockfile v1\n\n\"@scope/pkg@^2.0.0\", \"@scope/pkg@^2.1.0\":\n  version \"2.1.0\"\n  resolved \"https://r\"\n\nleft-pad@^1.0.0:\n  version \"1.3.0\"\n";
    assert_eq!(
        pins(Format::YarnLock, v1),
        vec![pair("@scope/pkg", "2.1.0"), pair("left-pad", "1.3.0")]
    );
    let berry = "__metadata:\n  version: 8\n\n\"left-pad@npm:^1.0.0\":\n  version: 1.3.0\n\n\"app@workspace:.\":\n  version: 0.0.0-use.local\n";
    assert_eq!(
        pins(Format::YarnLock, berry),
        vec![pair("left-pad", "1.3.0")]
    );
}

#[test]
fn uv_and_poetry_keep_index_packages() {
    let uv = r#"
version = 1
[[package]]
name = "requests"
version = "2.32.3"
source = { registry = "https://pypi.org/simple" }
[[package]]
name = "app"
version = "0.1.0"
source = { editable = "." }
"#;
    assert_eq!(pins(Format::UvLock, uv), vec![pair("requests", "2.32.3")]);
    let poetry = r#"
[[package]]
name = "requests"
version = "2.32.3"
[[package]]
name = "lib"
version = "1.0.0"
[package.source]
type = "git"
url = "https://x"
"#;
    assert_eq!(
        pins(Format::PoetryLock, poetry),
        vec![pair("requests", "2.32.3")]
    );
}

#[test]
fn requirements_keep_exact_pins_only() {
    let text = "requests==2.32.3 \\\n    --hash=sha256:abc\nflask[async]==3.0.0 ; python_version >= '3.8'\nnumpy>=1.0\n-r other.txt\ntorch==2.*\nweird===1.0\n# comment==1\n";
    assert_eq!(
        pins(Format::Requirements, text),
        vec![pair("flask", "3.0.0"), pair("requests", "2.32.3")]
    );
}

#[test]
fn go_sum_reads_each_version_once() {
    let text = "golang.org/x/mod v0.20.0 h1:abc=\ngolang.org/x/mod v0.20.0/go.mod h1:def=\ngithub.com/x/y v1.0.0+incompatible h1:g=\n";
    assert_eq!(
        pins(Format::GoSum, text),
        vec![
            pair("github.com/x/y", "v1.0.0+incompatible"),
            pair("golang.org/x/mod", "v0.20.0"),
        ]
    );
}

#[test]
fn conda_lock_keeps_conda_packages() {
    let text = "version: 1\npackage:\n  - name: numpy\n    version: 1.26.4\n    manager: conda\n    platform: linux-64\n  - name: numpy\n    version: 1.26.4\n    manager: conda\n    platform: osx-arm64\n  - name: requests\n    version: 2.32.3\n    manager: pip\n";
    assert_eq!(pins(Format::CondaLock, text), vec![pair("numpy", "1.26.4")]);
}

#[test]
fn composer_lock_skips_dev_branches() {
    let text = r#"{"packages": [{"name": "monolog/monolog", "version": "3.7.0"}],
        "packages-dev": [{"name": "x/y", "version": "dev-main"}, {"name": "phpunit/phpunit", "version": "11.0.0"}]}"#;
    assert_eq!(
        pins(Format::ComposerLock, text),
        vec![
            pair("monolog/monolog", "3.7.0"),
            pair("phpunit/phpunit", "11.0.0")
        ]
    );
}

#[test]
fn gemfile_lock_reads_gem_specs_not_their_requirements() {
    let text = "GIT\n  remote: https://x\n  specs:\n    forked (0.1.0)\n\nGEM\n  remote: https://rubygems.org/\n  specs:\n    rack (3.1.7)\n    rails (7.2.0)\n      rack (>= 2.2)\n\nPLATFORMS\n  ruby\n";
    assert_eq!(
        pins(Format::GemfileLock, text),
        vec![pair("rack", "3.1.7"), pair("rails", "7.2.0")]
    );
}

#[test]
fn nuget_lock_skips_project_references() {
    let text = r#"{"version": 1, "dependencies": {"net8.0": {
        "Newtonsoft.Json": {"type": "Direct", "requested": "[13.0.3, )", "resolved": "13.0.3"},
        "Lib": {"type": "Project"}}}}"#;
    assert_eq!(
        pins(Format::NugetLock, text),
        vec![pair("Newtonsoft.Json", "13.0.3")]
    );
}

#[test]
fn terraform_lock_names_providers_as_the_registry_does() {
    let text = "provider \"registry.terraform.io/hashicorp/aws\" {\n  version     = \"5.60.0\"\n  constraints = \"~> 5.0\"\n  hashes = [\n    \"h1:x\",\n  ]\n}\n";
    assert_eq!(
        pins(Format::TerraformLock, text),
        vec![pair("providers/hashicorp/aws", "5.60.0")]
    );
}

#[test]
fn chart_lock_reads_dependencies() {
    let text = "dependencies:\n- name: redis\n  repository: oci://r\n  version: 19.6.0\ndigest: sha256:x\n";
    assert_eq!(pins(Format::ChartLock, text), vec![pair("redis", "19.6.0")]);
}

#[test]
fn toolchain_files_pin_only_exact_versions() {
    assert_eq!(
        pins(Format::Nvmrc, "20.11.0\n"),
        vec![pair("node", "v20.11.0")]
    );
    assert_eq!(
        pins(Format::Nvmrc, "v20.11.0"),
        vec![pair("node", "v20.11.0")]
    );
    assert!(pins(Format::Nvmrc, "lts/*").is_empty());
    assert_eq!(
        pins(Format::Sdkmanrc, "# java\njava=21.0.2-tem\nmaven=3.9.6\n"),
        vec![pair("java", "21.0.2-tem"), pair("maven", "3.9.6")]
    );
    assert_eq!(
        pins(Format::RustToolchain, "[toolchain]\nchannel = \"1.80.0\"\n"),
        vec![pair("rust", "1.80.0")]
    );
    assert!(pins(Format::RustToolchain, "[toolchain]\nchannel = \"stable\"\n").is_empty());
}

/// T9: a file over the cap is refused before it is parsed.
#[test]
fn an_oversized_lockfile_is_refused_unread() {
    let big = vec![b' '; MAX_LOCKFILE_BYTES + 1];
    assert_eq!(
        read(Format::GoSum, &big),
        Err(LockfileError::TooLarge { file: "go.sum" })
    );
}

#[test]
fn a_file_of_another_format_is_an_error_not_a_panic() {
    for format in Format::ALL {
        let _ = read(format, b"\xff\xfe{[ not a lockfile \x00");
    }
    assert!(matches!(
        read(Format::CargoLock, b"[[package"),
        Err(LockfileError::Malformed { .. })
    ));
}

#[test]
fn file_names_map_to_formats_and_back() {
    for format in Format::ALL {
        assert_eq!(Format::from_file_name(format.file()), Some(format));
    }
    assert_eq!(
        Format::from_file_name("requirements-dev.txt"),
        Some(Format::Requirements)
    );
    assert_eq!(Format::from_file_name("README.md"), None);
}
