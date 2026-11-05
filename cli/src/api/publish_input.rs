//! Reading a publish input into what a kind's upload route takes
//! (RFC 0040 §6.3): a Go module directory into its module zip, a Maven pom
//! into its coordinate, a Terraform provider directory into its platforms.
//! Pure functions of the filesystem, so each is tested without a server.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// `golang.org/x/mod/zip`'s limit on the unpacked size of a module.
const MAX_MODULE_BYTES: u64 = 500 << 20;
/// Directories a module zip never carries: version control, and the
/// `vendor` tree the module proxy protocol leaves out.
const SKIPPED_DIRS: &[&str] = &[".git", ".hg", ".svn", ".bzr", "vendor"];

/// The `module` line of a `go.mod`.
pub fn go_module_path(go_mod: &str) -> Option<String> {
    go_mod.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("module")?;
        let path = rest.trim().trim_matches('"');
        (!path.is_empty() && rest.starts_with([' ', '\t', '"'])).then(|| path.to_owned())
    })
}

/// Zip a module directory in the layout the module proxy serves:
/// every entry under `{module}@{version}/`. Symlinks and irregular files are
/// left out, as are nested modules (a subdirectory with its own `go.mod`) and
/// the directories in [`SKIPPED_DIRS`]. Returns the module path and the zip.
pub fn go_module_zip(dir: &Path, version: &str) -> Result<(String, Vec<u8>)> {
    if !version.starts_with('v') {
        bail!("a Go module version starts with `v` (v1.2.3), got `{version}`");
    }
    let go_mod = std::fs::read_to_string(dir.join("go.mod"))
        .with_context(|| format!("{} has no go.mod", dir.display()))?;
    let module = go_module_path(&go_mod)
        .with_context(|| format!("{}/go.mod names no module", dir.display()))?;
    let mut files = Vec::new();
    collect_module_files(dir, dir, &mut files)?;
    files.sort();

    let prefix = format!("{module}@{version}/");
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut buf);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut total = 0u64;
    for rel in files {
        let path = dir.join(&rel);
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        total += bytes.len() as u64;
        if total > MAX_MODULE_BYTES {
            bail!(
                "{} is over the module limit of 500 MiB unpacked",
                dir.display()
            );
        }
        let name = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        zip.start_file(format!("{prefix}{name}"), opts)?;
        std::io::Write::write_all(&mut zip, &bytes)?;
    }
    zip.finish()?;
    Ok((module, buf.into_inner()))
}

fn collect_module_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        // `DirEntry::file_type` does not follow a link: one is neither
        // followed nor carried.
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_dir() {
            let name = entry.file_name();
            if SKIPPED_DIRS.iter().any(|s| name == *s) || path.join("go.mod").exists() {
                continue;
            }
            collect_module_files(root, &path, out)?;
        } else if kind.is_file() {
            out.push(path.strip_prefix(root)?.to_path_buf());
        }
    }
    Ok(())
}

/// A Maven coordinate read from a pom.
#[derive(Debug, PartialEq, Eq)]
pub struct PomCoordinate {
    pub group_id: String,
    pub artifact_id: String,
    pub version: String,
    /// `<packaging>`, `jar` when absent.
    pub packaging: String,
}

impl PomCoordinate {
    /// The repository directory of this version: `org/slf4j/slf4j-api/2.0.9`.
    pub fn dir(&self) -> String {
        format!(
            "{}/{}/{}",
            self.group_id.replace('.', "/"),
            self.artifact_id,
            self.version
        )
    }

    /// The `group:artifact` name the registry files the version under.
    pub fn name(&self) -> String {
        format!("{}:{}", self.group_id, self.artifact_id)
    }
}

/// The project's own `groupId`/`artifactId`/`version`, as top-level
/// elements only: the sections that carry other coordinates, and comments,
/// are cut first. A coordinate inherited from `<parent>` is refused, because
/// the registry reads the pom's own elements and nothing else — the jar would
/// be stored and the pom then refused, leaving a version with no pom.
pub fn pom_coordinate(pom: &str) -> Option<PomCoordinate> {
    let mut own = pom.to_owned();
    while let Some(start) = own.find("<!--") {
        let end = own[start..]
            .find("-->")
            .map_or(own.len(), |e| start + e + 3);
        own.replace_range(start..end, "");
    }
    for nested in [
        "parent",
        "dependencies",
        "dependencyManagement",
        "build",
        "profiles",
        "reporting",
        "modules",
        "plugins",
        "pluginRepositories",
        "repositories",
        "distributionManagement",
        "properties",
    ] {
        while let Some((start, end)) = span(&own, nested) {
            own.replace_range(start..end, "");
        }
    }
    let group_id = element(&own, "groupId")?;
    let artifact_id = element(&own, "artifactId")?;
    let version = element(&own, "version")?;
    // A `${…}` property is the build's to resolve; publishing it verbatim
    // would file the artifact under a literal placeholder.
    if [&group_id, &artifact_id, &version]
        .iter()
        .any(|v| v.contains("${"))
    {
        return None;
    }
    Some(PomCoordinate {
        group_id,
        artifact_id,
        version,
        packaging: element(&own, "packaging").unwrap_or_else(|| "jar".into()),
    })
}

fn span(xml: &str, tag: &str) -> Option<(usize, usize)> {
    let open = xml.find(&format!("<{tag}>"))?;
    let close_tag = format!("</{tag}>");
    let close = xml[open..].find(&close_tag)? + open + close_tag.len();
    Some((open, close))
}

fn element(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&format!("</{tag}>"))? + start;
    let value = xml[start..end].trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// One platform archive of a Terraform provider release.
#[derive(Debug, PartialEq, Eq)]
pub struct ProviderPlatform {
    pub os: String,
    pub arch: String,
    pub path: PathBuf,
}

/// The `terraform-provider-{type}_{version}_{os}_{arch}.zip` files of a
/// release directory, and the version they agree on.
pub fn provider_platforms(dir: &Path) -> Result<(String, Vec<ProviderPlatform>)> {
    let mut version: Option<String> = None;
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name
            .strip_prefix("terraform-provider-")
            .and_then(|s| s.strip_suffix(".zip"))
        else {
            continue;
        };
        let parts: Vec<&str> = stem.split('_').collect();
        let [_ptype, v, os, arch] = parts.as_slice() else {
            bail!("{name}: expected terraform-provider-<type>_<version>_<os>_<arch>.zip");
        };
        match &version {
            Some(seen) if seen != v => {
                bail!("{name} is version {v}, but the directory also holds {seen}")
            }
            _ => version = Some((*v).to_owned()),
        }
        out.push(ProviderPlatform {
            os: (*os).to_owned(),
            arch: (*arch).to_owned(),
            path: entry.path(),
        });
    }
    out.sort_by(|a, b| (&a.os, &a.arch).cmp(&(&b.os, &b.arch)));
    let version = version.with_context(|| {
        format!(
            "{} holds no terraform-provider-<type>_<version>_<os>_<arch>.zip",
            dir.display()
        )
    })?;
    Ok((version, out))
}

/// The release's checksum list and its signature, as goreleaser names them
/// (`terraform-provider-{type}_{version}_SHA256SUMS` and `….sig`): what
/// `terraform init` verifies a provider published here against (issue #281).
pub fn provider_shasums(dir: &Path, version: &str) -> Result<(Option<PathBuf>, Option<PathBuf>)> {
    let (mut sums, mut sig) = (None, None);
    let sums_suffix = format!("_{version}_SHA256SUMS");
    let sig_suffix = format!("{sums_suffix}.sig");
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("terraform-provider-") {
            continue;
        }
        if name.ends_with(&sig_suffix) {
            sig = Some(entry.path());
        } else if name.ends_with(&sums_suffix) {
            sums = Some(entry.path());
        }
    }
    Ok((sums, sig))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_module_line_is_read() {
        assert_eq!(
            go_module_path("// c\nmodule example.com/mod\n\ngo 1.22\n").as_deref(),
            Some("example.com/mod")
        );
        assert_eq!(
            go_module_path("module \"x.io/y\"").as_deref(),
            Some("x.io/y")
        );
        assert_eq!(go_module_path("modules x\n"), None);
        assert_eq!(go_module_path("go 1.22\n"), None);
    }

    #[test]
    fn a_module_zip_carries_its_files_under_the_version_prefix_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::write(d.join("go.mod"), "module example.com/mod\n").unwrap();
        std::fs::write(d.join("a.go"), "package mod\n").unwrap();
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("sub/b.go"), "package sub\n").unwrap();
        for skipped in [".git", "vendor", "nested"] {
            std::fs::create_dir_all(d.join(skipped)).unwrap();
            std::fs::write(d.join(skipped).join("x.go"), "x").unwrap();
        }
        std::fs::write(d.join("nested/go.mod"), "module example.com/mod/nested\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/passwd", d.join("link")).unwrap();

        let (module, zip) = go_module_zip(d, "v0.1.0").unwrap();
        assert_eq!(module, "example.com/mod");
        let archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).unwrap();
        let mut names: Vec<String> = archive
            .file_names()
            .map(|n| n.unwrap().into_owned())
            .collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "example.com/mod@v0.1.0/a.go",
                "example.com/mod@v0.1.0/go.mod",
                "example.com/mod@v0.1.0/sub/b.go"
            ]
        );
        assert!(go_module_zip(d, "0.1.0").is_err(), "a version needs its v");
    }

    #[test]
    fn a_pom_coordinate_ignores_dependencies_comments_and_the_parent() {
        let pom = r#"<project>
  <parent><groupId>org.parent</groupId><artifactId>p</artifactId><version>9</version></parent>
  <groupId>org.app</groupId>
  <artifactId>app</artifactId>
  <!-- <version>0.9</version> -->
  <version>1.0</version>
  <packaging>war</packaging>
  <dependencies><dependency><groupId>junit</groupId><artifactId>junit</artifactId>
    <version>4.13</version></dependency></dependencies>
</project>"#;
        let c = pom_coordinate(pom).unwrap();
        assert_eq!(
            c,
            PomCoordinate {
                group_id: "org.app".into(),
                artifact_id: "app".into(),
                version: "1.0".into(),
                packaging: "war".into(),
            }
        );
        assert_eq!(c.dir(), "org/app/app/1.0");
        assert_eq!(c.name(), "org.app:app");

        // Inherited from the parent: the registry would refuse the pom after
        // the jar was stored, so the CLI refuses it before sending anything.
        let inherited = r#"<project>
  <parent><groupId>org.parent</groupId><artifactId>p</artifactId><version>9</version></parent>
  <artifactId>app</artifactId>
</project>"#;
        assert_eq!(pom_coordinate(inherited), None);

        let own = "<project><groupId>g.h</groupId><artifactId>a</artifactId>\
                   <version>1.0</version></project>";
        assert_eq!(pom_coordinate(own).unwrap().dir(), "g/h/a/1.0");
        let templated = "<project><groupId>g</groupId><artifactId>a</artifactId>\
                         <version>${revision}</version></project>";
        assert_eq!(pom_coordinate(templated), None);
    }

    #[test]
    fn provider_platforms_agree_on_one_version() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        for f in [
            "terraform-provider-null_1.2.0_linux_amd64.zip",
            "terraform-provider-null_1.2.0_darwin_arm64.zip",
            "terraform-provider-null_1.2.0_SHA256SUMS",
        ] {
            std::fs::write(d.join(f), b"z").unwrap();
        }
        let (v, ps) = provider_platforms(d).unwrap();
        assert_eq!(v, "1.2.0");
        let (sums, sig) = provider_shasums(d, &v).unwrap();
        assert_eq!(
            sums.unwrap().file_name().unwrap(),
            "terraform-provider-null_1.2.0_SHA256SUMS"
        );
        assert_eq!(sig, None);
        std::fs::write(d.join("terraform-provider-null_1.2.0_SHA256SUMS.sig"), b"s").unwrap();
        assert!(provider_shasums(d, &v).unwrap().1.is_some());
        assert_eq!(
            ps.iter()
                .map(|p| format!("{}_{}", p.os, p.arch))
                .collect::<Vec<_>>(),
            ["darwin_arm64", "linux_amd64"]
        );
        std::fs::write(
            d.join("terraform-provider-null_1.3.0_linux_arm64.zip"),
            b"z",
        )
        .unwrap();
        assert!(
            provider_platforms(d).is_err(),
            "two versions in one directory"
        );
    }
}
