use std::path::{Path, PathBuf};

use crate::api::client_setup::{render_body, TargetId};
use crate::api::registry::RegistryTargets;

pub struct ProjectDetection {
    pub registry_type: &'static str,
    pub package_name: Option<String>,
    /// Multi-line instructions shown in the TUI detail pane.
    pub instructions: String,
    /// Path relative to the scan root (empty string = root itself).
    pub relative_path: String,
    /// Base URL the instructions were built against — a configured registry's
    /// own URL, or the `{server}/proxy/<registry>` placeholder.
    pub base_url: String,
    /// The configured registry the instructions point at, when the server had
    /// one of a matching type; `None` when they still carry the placeholder.
    pub registry_name: Option<String>,
}

/// The scanner's label → the `type` value a registry is configured with.
///
/// Identical apart from Go, whose manifest is `go.mod` but whose registry type
/// is `goproxy` (`api::suggest::manifest_registry` maps the same pair).
pub fn api_registry_type(detected: &str) -> &str {
    match detected {
        "gomodules" => "goproxy",
        other => other,
    }
}

/// Directories that are never entered during recursive scanning.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "vendor",
    ".git",
    ".github",
    ".hg",
    "dist",
    ".build",
    "__pycache__",
    ".tox",
    ".venv",
    "venv",
    ".mypy_cache",
];

/// Recursively scan `root` (up to `max_depth` levels of subdirectories) for
/// known project manifests and return one [`ProjectDetection`] per hit.
/// `max_depth = 0` restricts the scan to the root directory itself.
///
/// `targets` decides the URL each instruction block points at: a configured
/// registry's own host when the server advertises one, the
/// `{server}/proxy/<registry>` placeholder otherwise.
pub fn scan_project_types(
    root: &Path,
    targets: &RegistryTargets,
    max_depth: usize,
) -> Vec<ProjectDetection> {
    scan_recursive(root, root, targets, max_depth)
}

/// Assemble one detection, resolving the registry for its type once so every
/// `detect_*` below is only a formatter.
fn detection(
    registry_type: &'static str,
    targets: &RegistryTargets,
    package_name: Option<String>,
    instructions: impl FnOnce(&str) -> String,
) -> ProjectDetection {
    let api_type = api_registry_type(registry_type);
    let base_url = targets.base_for(api_type);
    ProjectDetection {
        registry_type,
        package_name,
        instructions: instructions(&base_url),
        relative_path: String::new(),
        registry_name: targets.registry_for(api_type).map(|r| r.name.clone()),
        base_url,
    }
}

/// Enumerate one directory level: returns (file_names, sorted_subdirs).
/// Subdirs are filtered by SKIP_DIRS and hidden-directory heuristic; only
/// included when `remaining_depth > 0`.
fn read_dir_entries(dir: &Path, remaining_depth: usize) -> (Vec<String>, Vec<PathBuf>) {
    let mut file_names: Vec<String> = Vec::new();
    let mut subdirs: Vec<PathBuf> = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("Warning: failed to read directory {}: {e}", dir.display());
            return (file_names, subdirs);
        }
    };
    for entry in entries.flatten() {
        match classify_entry(&entry, remaining_depth) {
            Some(DirEntryKind::File(name)) => file_names.push(name),
            Some(DirEntryKind::Dir(path)) => subdirs.push(path),
            None => {}
        }
    }
    subdirs.sort();
    (file_names, subdirs)
}

/// Result of classifying a single directory entry.
enum DirEntryKind {
    File(String),
    Dir(PathBuf),
}

/// Classify a single directory entry, applying the `SKIP_DIRS` /
/// hidden-directory filter to subdirectories.
fn classify_entry(entry: &std::fs::DirEntry, remaining_depth: usize) -> Option<DirEntryKind> {
    let ft = entry.file_type().ok()?;
    let path = entry.path();
    let name = path.file_name().and_then(std::ffi::OsStr::to_str)?;
    if ft.is_file() {
        Some(DirEntryKind::File(name.to_string()))
    } else if ft.is_dir() && remaining_depth > 0 && !is_skipped_dir(name) {
        Some(DirEntryKind::Dir(path))
    } else {
        None
    }
}

/// Hidden directories (dotfiles) and entries in [`SKIP_DIRS`] are never
/// descended into during recursive scanning.
fn is_skipped_dir(name: &str) -> bool {
    name.starts_with('.') || SKIP_DIRS.contains(&name)
}

fn scan_recursive(
    root: &Path,
    dir: &Path,
    targets: &RegistryTargets,
    remaining_depth: usize,
) -> Vec<ProjectDetection> {
    let rel = dir
        .strip_prefix(root)
        .unwrap_or(Path::new(""))
        .to_string_lossy()
        .replace('\\', "/");

    let (file_names, subdirs) = read_dir_entries(dir, remaining_depth);

    let mut out: Vec<ProjectDetection> = detect_project_types_in(dir, targets, &file_names)
        .into_iter()
        .map(|mut d| {
            d.relative_path = rel.clone();
            d
        })
        .collect();

    for sub in subdirs {
        out.extend(scan_recursive(root, &sub, targets, remaining_depth - 1));
    }

    out
}

fn detect_project_types_in(
    dir: &Path,
    targets: &RegistryTargets,
    dir_names: &[String],
) -> Vec<ProjectDetection> {
    let mut out = Vec::new();

    if let Some(det) = detect_cargo(dir, targets) {
        out.push(det);
    }
    if let Some(det) = detect_gomodules(dir, targets) {
        out.push(det);
    }
    if let Some(det) = detect_npm(dir, targets) {
        out.push(det);
    }
    if let Some(det) = detect_pypi(dir, targets) {
        out.push(det);
    }
    if let Some(det) = detect_maven(dir, targets) {
        out.push(det);
    }
    if let Some(det) = detect_composer(dir, targets) {
        out.push(det);
    }
    if let Some(det) = detect_rubygems(dir_names, targets) {
        out.push(det);
    }
    if let Some(det) = detect_nuget(dir_names, targets) {
        out.push(det);
    }
    if let Some(det) = detect_terraform(dir, dir_names, targets) {
        out.push(det);
    }
    if let Some(det) = detect_conda(dir, targets) {
        out.push(det);
    }

    out
}

fn detect_cargo(dir: &Path, targets: &RegistryTargets) -> Option<ProjectDetection> {
    let cargo_toml = dir.join("Cargo.toml");
    if !cargo_toml.exists() {
        return None;
    }
    let name = read_toml_field(&cargo_toml, &["package", "name"]);
    let pkg = name.as_deref().unwrap_or("<package>").to_string();
    Some(detection("cargo", targets, name, |base| {
        format!(
            "Registry type : cargo\n\
             Package       : {pkg}\n\
             \n\
             ~/.cargo/config.toml:\n\
             {body}\n\
             \n\
             Publish:\n\
             cargo publish --registry batlehub",
            body = render_body(TargetId::Cargo, base)
        )
    }))
}

fn detect_gomodules(dir: &Path, targets: &RegistryTargets) -> Option<ProjectDetection> {
    let go_mod = dir.join("go.mod");
    if !go_mod.exists() {
        return None;
    }
    let name = read_gomod_module(&go_mod);
    let pkg = name.as_deref().unwrap_or("<module>").to_string();
    Some(detection("gomodules", targets, name, |base| {
        format!(
            "Registry type : gomodules\n\
             Module        : {pkg}\n\
             \n\
             Environment:\n\
             export {}\n\
             \n\
             Use:\n\
             go get {pkg}",
            render_body(TargetId::Go, base)
        )
    }))
}

fn detect_npm(dir: &Path, targets: &RegistryTargets) -> Option<ProjectDetection> {
    let pkg_json = dir.join("package.json");
    if !pkg_json.exists() {
        return None;
    }
    let name = read_json_field(&pkg_json, "name");
    let pkg = name.as_deref().unwrap_or("<package>").to_string();
    Some(detection("npm", targets, name, |base| {
        format!(
            "Registry type : npm\n\
             Package       : {pkg}\n\
             \n\
             .npmrc:\n\
             {}\n\
             \n\
             Publish:\n\
             npm publish",
            render_body(TargetId::Npmrc, base)
        )
    }))
}

fn detect_maven(dir: &Path, targets: &RegistryTargets) -> Option<ProjectDetection> {
    let pom_xml = dir.join("pom.xml");
    if !pom_xml.exists() {
        return None;
    }
    let name = read_xml_tag(&pom_xml, "artifactId");
    let pkg = name.as_deref().unwrap_or("<artifactId>").to_string();
    Some(detection("maven", targets, name, |base| {
        format!(
            "Registry type : maven\n\
             Artifact      : {pkg}\n\
             \n\
             settings.xml:\n\
             <repository>\n\
               <id>batlehub</id>\n\
               <url>{base}/maven2/</url>\n\
             </repository>\n\
             \n\
             Publish:\n\
             mvn deploy"
        )
    }))
}

fn detect_composer(dir: &Path, targets: &RegistryTargets) -> Option<ProjectDetection> {
    let composer_json = dir.join("composer.json");
    if !composer_json.exists() {
        return None;
    }
    let name = read_json_field(&composer_json, "name");
    let pkg = name.as_deref().unwrap_or("<package>").to_string();
    Some(detection("composer", targets, name, |base| {
        format!(
            "Registry type : composer\n\
             Package       : {pkg}\n\
             \n\
             composer.json:\n\
             \"repositories\": [{{\n\
               \"type\": \"composer\",\n\
               \"url\": \"{base}/\"\n\
             }}]"
        )
    }))
}

fn detect_rubygems(dir_names: &[String], targets: &RegistryTargets) -> Option<ProjectDetection> {
    let has_gemspec = dir_names.iter().any(|n| n.ends_with(".gemspec"));
    let has_gemfile = dir_names.iter().any(|n| n == "Gemfile");
    if !has_gemspec && !has_gemfile {
        return None;
    }
    let name = dir_names
        .iter()
        .find(|n| n.ends_with(".gemspec"))
        .and_then(|n| n.strip_suffix(".gemspec"))
        .map(str::to_string);
    let pkg = name.as_deref().unwrap_or("<gem>").to_string();
    Some(detection("rubygems", targets, name, |base| {
        format!(
            "Registry type : rubygems\n\
             Gem           : {pkg}\n\
             \n\
             ~/.gemrc:\n\
             :sources:\n\
             - {base}/\n\
             \n\
             Publish:\n\
             gem push *.gem --host {base}"
        )
    }))
}

fn detect_terraform(
    dir: &Path,
    dir_names: &[String],
    targets: &RegistryTargets,
) -> Option<ProjectDetection> {
    let has_tf = dir_names.iter().any(|n| n.ends_with(".tf"));
    if !has_tf {
        return None;
    }
    let name = dir
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_string);
    Some(detection("terraform", targets, name, |base| {
        format!(
            "Registry type : terraform\n\
             \n\
             ~/.terraformrc:\n\
             provider_installation {{\n\
               network_mirror {{\n\
                 url = \"{base}/\"\n\
               }}\n\
             }}"
        )
    }))
}

fn detect_conda(dir: &Path, targets: &RegistryTargets) -> Option<ProjectDetection> {
    let env_yml = dir.join("environment.yml");
    if !env_yml.exists() {
        return None;
    }
    let name = grep_key(&env_yml, "name:");
    let pkg = name.as_deref().unwrap_or("<env>").to_string();
    Some(detection("conda", targets, name, |base| {
        format!(
            "Registry type : conda\n\
             Environment   : {pkg}\n\
             \n\
             ~/.condarc:\n\
             channels:\n\
               - {base}\n\
             \n\
             Publish:\n\
             batlehub-cli publish *.conda"
        )
    }))
}

fn detect_pypi(dir: &Path, targets: &RegistryTargets) -> Option<ProjectDetection> {
    let pyproject = dir.join("pyproject.toml");
    let setup_py = dir.join("setup.py");
    if !pyproject.exists() && !setup_py.exists() {
        return None;
    }
    let name = read_toml_field(&pyproject, &["project", "name"])
        .or_else(|| read_toml_field(&pyproject, &["tool", "poetry", "name"]));
    let pkg = name.as_deref().unwrap_or("<package>").to_string();
    Some(detection("pypi", targets, name, |base| {
        format!(
            "Registry type : pypi\n\
             Package       : {pkg}\n\
             \n\
             pip.conf / pip.ini:\n\
             {}\n\
             \n\
             Publish:\n\
             twine upload dist/*",
            render_body(TargetId::Pip, base)
        )
    }))
}

fn detect_nuget(dir_names: &[String], targets: &RegistryTargets) -> Option<ProjectDetection> {
    let has_nuspec = dir_names.iter().any(|n| n.ends_with(".nuspec"));
    let has_csproj = dir_names.iter().any(|n| n.ends_with(".csproj"));
    if !has_nuspec && !has_csproj {
        return None;
    }
    let name = dir_names
        .iter()
        .find(|n| n.ends_with(".nuspec"))
        .and_then(|n| n.strip_suffix(".nuspec"))
        .map(str::to_string)
        .or_else(|| {
            dir_names
                .iter()
                .find(|n| n.ends_with(".csproj"))
                .and_then(|n| n.strip_suffix(".csproj"))
                .map(str::to_string)
        });
    let pkg = name.as_deref().unwrap_or("<package>").to_string();
    Some(detection("nuget", targets, name, |base| {
        format!(
            "Registry type : nuget\n\
             Package       : {pkg}\n\
             \n\
             Add NuGet source:\n\
             dotnet nuget add source \\\n\
               {base}/nuget/v3/index.json \\\n\
               --name batlehub\n\
             \n\
             Publish:\n\
             dotnet nuget push *.nupkg --source batlehub"
        )
    }))
}

// ── Manifest parsing helpers ───────────────────────────────────────────────

fn read_toml_field(path: &Path, keys: &[&str]) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let value: toml::Value = toml::from_str(&content).ok()?;
    let mut cur = &value;
    for key in keys {
        cur = cur.get(key)?;
    }
    cur.as_str().map(str::to_string)
}

fn read_gomod_module(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let module_path = content
        .lines()
        .find(|l| l.starts_with("module "))?
        .strip_prefix("module ")?
        .trim()
        .to_string();
    // Return the last segment as a short name
    let short = module_path.rsplit('/').next().unwrap_or(&module_path);
    Some(short.to_string())
}

fn read_json_field(path: &Path, field: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    value.get(field)?.as_str().map(str::to_string)
}

fn read_xml_tag(path: &Path, tag: &str) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    // Strip <parent>…</parent> so that a parent-POM's <artifactId> is not
    // returned instead of the project's own top-level element.
    let content = strip_xml_block(&raw, "parent");
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = content.find(&open)? + open.len();
    let end = content[start..].find(&close)?;
    Some(content[start..start + end].trim().to_string())
}

fn strip_xml_block(content: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    match (content.find(&open), content.find(&close)) {
        (Some(s), Some(e)) if s < e => {
            format!("{}{}", &content[..s], &content[e + close.len()..])
        }
        _ => content.to_string(),
    }
}

fn grep_key(path: &Path, prefix: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    content.lines().find_map(|line| {
        line.strip_prefix(prefix)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::registry::RegistryInfo;
    use std::fs;
    use tempfile::TempDir;

    /// The offline case: no registry list, so every instruction keeps the
    /// `<registry>` placeholder it has always printed.
    fn targets() -> RegistryTargets<'static> {
        RegistryTargets::new("http://localhost:8080", &[])
    }

    #[test]
    fn detects_cargo_toml() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"my-crate\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let results = scan_project_types(dir.path(), &targets(), 0);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].registry_type, "cargo");
        assert_eq!(results[0].package_name.as_deref(), Some("my-crate"));
        assert!(results[0].instructions.contains("cargo publish"));
    }

    #[test]
    fn detects_gomod() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            "module github.com/example/myapp\n\ngo 1.21\n",
        )
        .unwrap();
        let results = scan_project_types(dir.path(), &targets(), 0);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].registry_type, "gomodules");
        assert_eq!(results[0].package_name.as_deref(), Some("myapp"));
    }

    #[test]
    fn detects_package_json() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"my-app","version":"1.0.0"}"#,
        )
        .unwrap();
        let results = scan_project_types(dir.path(), &targets(), 0);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].registry_type, "npm");
        assert_eq!(results[0].package_name.as_deref(), Some("my-app"));
    }

    #[test]
    fn empty_dir_returns_nothing() {
        let dir = TempDir::new().unwrap();
        let results = scan_project_types(dir.path(), &targets(), 0);
        assert!(results.is_empty());
    }

    #[test]
    fn detects_pom_xml() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("pom.xml"),
            "<project><artifactId>my-lib</artifactId></project>",
        )
        .unwrap();
        let results = scan_project_types(dir.path(), &targets(), 0);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].registry_type, "maven");
        assert_eq!(results[0].package_name.as_deref(), Some("my-lib"));
        assert!(results[0].instructions.contains("mvn deploy"));
    }

    #[test]
    fn detects_nuspec() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("MyPkg.nuspec"), "<package/>").unwrap();
        let results = scan_project_types(dir.path(), &targets(), 0);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].registry_type, "nuget");
        assert_eq!(results[0].package_name.as_deref(), Some("MyPkg"));
        assert!(results[0].instructions.contains("dotnet nuget push"));
    }

    #[test]
    fn detects_tf_file() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("main.tf"), "provider \"aws\" {}").unwrap();
        let results = scan_project_types(dir.path(), &targets(), 0);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].registry_type, "terraform");
        assert!(results[0].instructions.contains("terraform"));
    }

    #[test]
    fn detects_environment_yml() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("environment.yml"),
            "name: myenv\ndependencies:\n  - numpy\n",
        )
        .unwrap();
        let results = scan_project_types(dir.path(), &targets(), 0);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].registry_type, "conda");
        assert_eq!(results[0].package_name.as_deref(), Some("myenv"));
    }

    #[test]
    fn detects_multiple_types_in_same_dir() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"my-crate\"\n",
        )
        .unwrap();
        fs::write(dir.path().join("package.json"), r#"{"name":"my-app"}"#).unwrap();
        let results = scan_project_types(dir.path(), &targets(), 0);
        assert_eq!(results.len(), 2);
        let types: Vec<&str> = results.iter().map(|r| r.registry_type).collect();
        assert!(types.contains(&"cargo"), "expected cargo in {types:?}");
        assert!(types.contains(&"npm"), "expected npm in {types:?}");
    }

    #[test]
    fn detects_composer_json() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("composer.json"),
            r#"{"name":"vendor/my-package"}"#,
        )
        .unwrap();
        let results = scan_project_types(dir.path(), &targets(), 0);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].registry_type, "composer");
        assert_eq!(
            results[0].package_name.as_deref(),
            Some("vendor/my-package")
        );
    }

    #[test]
    fn detects_gemspec() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("my_gem.gemspec"),
            "Gem::Specification.new do |s| end",
        )
        .unwrap();
        let results = scan_project_types(dir.path(), &targets(), 0);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].registry_type, "rubygems");
        assert_eq!(results[0].package_name.as_deref(), Some("my_gem"));
    }

    fn registry(name: &str, ty: &str, public_url: Option<&str>) -> RegistryInfo {
        RegistryInfo {
            name: name.to_owned(),
            registry_type: ty.to_owned(),
            mode: "proxy".to_owned(),
            public_url: public_url.map(str::to_owned),
        }
    }

    /// With nothing configured, the output is byte-for-byte what it was before
    /// registries were wired in.
    #[test]
    fn offline_scan_keeps_the_registry_placeholder() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("package.json"), r#"{"name":"my-app"}"#).unwrap();
        let results = scan_project_types(dir.path(), &targets(), 0);
        assert!(results[0]
            .instructions
            .contains("registry=http://localhost:8080/proxy/<registry>/"));
        assert_eq!(results[0].registry_name, None);
    }

    /// A configured registry replaces the placeholder with its real name.
    #[test]
    fn scan_uses_a_configured_registry_name() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("package.json"), r#"{"name":"my-app"}"#).unwrap();
        let registries = vec![registry("npm1", "npm", None)];
        let targets = RegistryTargets::new("https://batlehub.example.com", &registries);

        let results = scan_project_types(dir.path(), &targets, 0);
        assert_eq!(results[0].registry_name.as_deref(), Some("npm1"));
        assert!(results[0]
            .instructions
            .contains("registry=https://batlehub.example.com/proxy/npm1/"));
    }

    /// Host-routed: the instructions must point at the registry's own subdomain
    /// and drop `/proxy/{name}`, which that host adds itself.
    #[test]
    fn scan_uses_the_registry_subdomain_when_one_is_advertised() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("package.json"), r#"{"name":"my-app"}"#).unwrap();
        fs::write(dir.path().join("go.mod"), "module example.com/app\n").unwrap();
        let registries = vec![
            registry("npm1", "npm", Some("https://npm1.batlehub.example.com")),
            // The scanner labels this `gomodules`; its registry type is `goproxy`.
            registry("go1", "goproxy", Some("https://go1.batlehub.example.com")),
        ];
        let targets = RegistryTargets::new("https://batlehub.example.com", &registries);

        let results = scan_project_types(dir.path(), &targets, 0);
        let npm = results.iter().find(|d| d.registry_type == "npm").unwrap();
        let go = results
            .iter()
            .find(|d| d.registry_type == "gomodules")
            .unwrap();

        assert_eq!(npm.base_url, "https://npm1.batlehub.example.com");
        assert!(npm
            .instructions
            .contains("registry=https://npm1.batlehub.example.com/"));
        assert_eq!(go.registry_name.as_deref(), Some("go1"));
        assert!(go
            .instructions
            .contains("export GOPROXY=https://go1.batlehub.example.com"));
        for det in &results {
            assert!(
                !det.instructions.contains("/proxy/"),
                "a host-routed registry re-adds /proxy/{{name}} itself: {}",
                det.instructions
            );
        }
    }
}
