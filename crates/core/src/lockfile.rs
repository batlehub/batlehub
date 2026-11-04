//! The lockfile readers behind `batlehub-cli check` (RFC 0040 §4.7, §6.5).
//!
//! Each reader is a pure function of a file's bytes to the versions it pins,
//! as `registry kind, name, version`: no I/O, no path built from the content,
//! and a size cap before anything is parsed (T9). A file that a project's own
//! tool wrote is trusted no further than that — a clone of anybody's
//! repository is a lockfile — so a reader skips what it cannot read rather
//! than failing on it, and only a file that is not its format at all is an
//! error. `fuzz/fuzz_targets/fuzz_cli_lockfile.rs` drives every one.
//!
//! What a reader keeps is what a client would download from a registry: a
//! workspace member, a path, a git source or a link is not pinned to a
//! registry version, so it is not asked about.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::entities::RegistryKind;

/// The largest lockfile `check` reads (§4.11).
pub const MAX_LOCKFILE_BYTES: usize = 16 * 1024 * 1024;

/// One pinned version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub kind: RegistryKind,
    pub name: String,
    pub version: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LockfileError {
    #[error("{file} is larger than {MAX_LOCKFILE_BYTES} bytes")]
    TooLarge { file: &'static str },
    #[error("{file} is not {format}: {detail}")]
    Malformed {
        file: &'static str,
        format: &'static str,
        detail: String,
    },
}

/// Every lockfile `check` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    CargoLock,
    PackageLock,
    PnpmLock,
    YarnLock,
    UvLock,
    PoetryLock,
    Requirements,
    GoSum,
    CondaLock,
    ComposerLock,
    GemfileLock,
    NugetLock,
    TerraformLock,
    ChartLock,
    Nvmrc,
    Sdkmanrc,
    RustToolchain,
}

impl Format {
    pub const ALL: [Format; 17] = [
        Format::CargoLock,
        Format::PackageLock,
        Format::PnpmLock,
        Format::YarnLock,
        Format::UvLock,
        Format::PoetryLock,
        Format::Requirements,
        Format::GoSum,
        Format::CondaLock,
        Format::ComposerLock,
        Format::GemfileLock,
        Format::NugetLock,
        Format::TerraformLock,
        Format::ChartLock,
        Format::Nvmrc,
        Format::Sdkmanrc,
        Format::RustToolchain,
    ];

    /// The format a file's name says it is: `requirements*.txt` by pattern,
    /// every other one by its exact name.
    pub fn from_file_name(name: &str) -> Option<Self> {
        Some(match name {
            "Cargo.lock" => Self::CargoLock,
            "package-lock.json" | "npm-shrinkwrap.json" => Self::PackageLock,
            "pnpm-lock.yaml" => Self::PnpmLock,
            "yarn.lock" => Self::YarnLock,
            "uv.lock" => Self::UvLock,
            "poetry.lock" => Self::PoetryLock,
            "go.sum" => Self::GoSum,
            "conda-lock.yml" | "conda-lock.yaml" => Self::CondaLock,
            "composer.lock" => Self::ComposerLock,
            "Gemfile.lock" => Self::GemfileLock,
            "packages.lock.json" => Self::NugetLock,
            ".terraform.lock.hcl" => Self::TerraformLock,
            "Chart.lock" => Self::ChartLock,
            ".nvmrc" => Self::Nvmrc,
            ".sdkmanrc" => Self::Sdkmanrc,
            "rust-toolchain.toml" => Self::RustToolchain,
            n if n.starts_with("requirements") && n.ends_with(".txt") => Self::Requirements,
            _ => return None,
        })
    }

    /// The file name, for messages.
    pub fn file(self) -> &'static str {
        match self {
            Self::CargoLock => "Cargo.lock",
            Self::PackageLock => "package-lock.json",
            Self::PnpmLock => "pnpm-lock.yaml",
            Self::YarnLock => "yarn.lock",
            Self::UvLock => "uv.lock",
            Self::PoetryLock => "poetry.lock",
            Self::Requirements => "requirements.txt",
            Self::GoSum => "go.sum",
            Self::CondaLock => "conda-lock.yml",
            Self::ComposerLock => "composer.lock",
            Self::GemfileLock => "Gemfile.lock",
            Self::NugetLock => "packages.lock.json",
            Self::TerraformLock => ".terraform.lock.hcl",
            Self::ChartLock => "Chart.lock",
            Self::Nvmrc => ".nvmrc",
            Self::Sdkmanrc => ".sdkmanrc",
            Self::RustToolchain => "rust-toolchain.toml",
        }
    }

    /// The registry kind a pin of this format is downloaded from.
    pub fn kind(self) -> RegistryKind {
        match self {
            Self::CargoLock => RegistryKind::Cargo,
            Self::PackageLock | Self::PnpmLock | Self::YarnLock => RegistryKind::Npm,
            Self::UvLock | Self::PoetryLock | Self::Requirements => RegistryKind::Pypi,
            Self::GoSum => RegistryKind::Goproxy,
            Self::CondaLock => RegistryKind::Conda,
            Self::ComposerLock => RegistryKind::Composer,
            Self::GemfileLock => RegistryKind::Rubygems,
            Self::NugetLock => RegistryKind::Nuget,
            Self::TerraformLock => RegistryKind::Terraform,
            Self::ChartLock => RegistryKind::Helm,
            Self::Nvmrc => RegistryKind::Nodedist,
            Self::Sdkmanrc => RegistryKind::Sdkman,
            Self::RustToolchain => RegistryKind::Rustup,
        }
    }
}

/// The versions `bytes` pins, sorted and without duplicates.
pub fn read(format: Format, bytes: &[u8]) -> Result<Vec<Pin>, LockfileError> {
    let file = format.file();
    if bytes.len() > MAX_LOCKFILE_BYTES {
        return Err(LockfileError::TooLarge { file });
    }
    let text = String::from_utf8_lossy(bytes);
    let malformed = |format: &'static str, detail: String| LockfileError::Malformed {
        file,
        format,
        detail,
    };
    let toml = |text: &str| {
        toml::from_str::<toml::Table>(text).map_err(|e| malformed("TOML", e.to_string()))
    };
    let json = |text: &str| {
        serde_json::from_str::<Value>(text).map_err(|e| malformed("JSON", e.to_string()))
    };
    let yaml = |text: &str| {
        serde_saphyr::from_str::<Value>(text).map_err(|e| malformed("YAML", e.to_string()))
    };
    let pairs: Vec<(String, String)> = match format {
        Format::CargoLock => cargo_lock(&toml(&text)?),
        Format::PackageLock => package_lock(&json(&text)?),
        Format::PnpmLock => pnpm_lock(&yaml(&text)?),
        Format::YarnLock => yarn_lock(&text),
        Format::UvLock => uv_lock(&toml(&text)?),
        Format::PoetryLock => poetry_lock(&toml(&text)?),
        Format::Requirements => requirements(&text),
        Format::GoSum => go_sum(&text),
        Format::CondaLock => conda_lock(&yaml(&text)?),
        Format::ComposerLock => composer_lock(&json(&text)?),
        Format::GemfileLock => gemfile_lock(&text),
        Format::NugetLock => nuget_lock(&json(&text)?),
        Format::TerraformLock => terraform_lock(&text),
        Format::ChartLock => chart_lock(&yaml(&text)?),
        Format::Nvmrc => nvmrc(&text),
        Format::Sdkmanrc => sdkmanrc(&text),
        Format::RustToolchain => rust_toolchain(&toml(&text)?),
    };
    let kind = format.kind();
    // One kind per file, so `(name, version)` is the identity.
    let pairs: BTreeSet<(String, String)> = pairs
        .into_iter()
        .filter(|(n, v)| !n.is_empty() && !v.is_empty())
        .collect();
    Ok(pairs
        .into_iter()
        .map(|(name, version)| Pin {
            kind,
            name,
            version,
        })
        .collect())
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// `[[package]]` tables, as TOML locks write them.
fn toml_packages(doc: &toml::Table) -> impl Iterator<Item = &toml::Table> {
    doc.get("package")
        .and_then(|p| p.as_array())
        .into_iter()
        .flatten()
        .filter_map(|p| p.as_table())
}

fn toml_str<'a>(t: &'a toml::Table, key: &str) -> Option<&'a str> {
    t.get(key).and_then(|v| v.as_str())
}

/// A package with a `registry+` or `sparse+` source; workspace members and
/// path dependencies have none, git ones a `git+` one.
fn cargo_lock(doc: &toml::Table) -> Vec<(String, String)> {
    toml_packages(doc)
        .filter(|p| {
            toml_str(p, "source")
                .is_some_and(|s| s.starts_with("registry+") || s.starts_with("sparse+"))
        })
        .filter_map(|p| Some((toml_str(p, "name")?.into(), toml_str(p, "version")?.into())))
        .collect()
}

/// lockfile v2/v3 `packages` (`node_modules/…` keys), else v1 `dependencies`.
fn package_lock(doc: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(packages) = doc.get("packages").and_then(Value::as_object) {
        for (path, p) in packages {
            let Some((_, name)) = path.rsplit_once("node_modules/") else {
                continue; // the root project, or a workspace directory
            };
            if p.get("link").and_then(Value::as_bool) == Some(true)
                || str_of(p, "resolved").is_some_and(|r| !r.starts_with("http"))
            {
                continue;
            }
            let name = str_of(p, "name").unwrap_or(name);
            if let Some(v) = str_of(p, "version") {
                out.push((name.to_owned(), v.to_owned()));
            }
        }
        return out;
    }
    fn walk(deps: &serde_json::Map<String, Value>, out: &mut Vec<(String, String)>, depth: usize) {
        if depth > 64 {
            return;
        }
        for (name, d) in deps {
            if let Some(v) = str_of(d, "version").filter(|v| !v.contains(':')) {
                out.push((name.clone(), v.to_owned()));
            }
            if let Some(inner) = d.get("dependencies").and_then(Value::as_object) {
                walk(inner, out, depth + 1);
            }
        }
    }
    if let Some(deps) = doc.get("dependencies").and_then(Value::as_object) {
        walk(deps, &mut out, 0);
    }
    out
}

/// `name@version`, a scoped name keeping its leading `@`.
fn split_at_version(spec: &str) -> Option<(&str, &str)> {
    // Past the first character, by its width: `spec[1..]` would split a
    // multibyte one.
    let start = spec.chars().next().map_or(0, char::len_utf8);
    let at = spec[start..].rfind('@')? + start;
    Some((&spec[..at], &spec[at + 1..]))
}

/// The keys of `packages:`: `name@1.0.0` (v9), `/name@1.0.0(peer@2)` (v6),
/// `/name/1.0.0` (v5).
fn pnpm_lock(doc: &Value) -> Vec<(String, String)> {
    let Some(packages) = doc.get("packages").and_then(Value::as_object) else {
        return Vec::new();
    };
    packages
        .keys()
        .filter_map(|key| {
            let key = key.strip_prefix('/').unwrap_or(key);
            let key = key.split('(').next().unwrap_or(key);
            match split_at_version(key) {
                Some((n, v)) => Some((n.to_owned(), v.to_owned())),
                None => key
                    .rsplit_once('/')
                    .map(|(n, v)| (n.to_owned(), v.to_owned())),
            }
        })
        .filter(|(_, v)| v.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .collect()
}

/// yarn v1 (`version "1.0.0"`) and berry (`version: 1.0.0`), one entry per
/// block; a berry `workspace:`, `patch:`, `link:` or `portal:` entry is not a
/// registry version.
fn yarn_lock(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut name: Option<String> = None;
    for line in text.lines() {
        if !line.starts_with(' ') && line.ends_with(':') && !line.starts_with('#') {
            let first = line.trim_end_matches(':').split(',').next().unwrap_or("");
            let spec = first.trim().trim_matches('"');
            // A range with a protocol other than `npm:` (berry's
            // `workspace:`, `patch:`, `link:`; a git or file URL) is not
            // resolved from a registry.
            name = match split_at_version(spec) {
                Some((n, range))
                    if !n.is_empty() && (!range.contains(':') || range.starts_with("npm:")) =>
                {
                    Some(n.to_owned())
                }
                _ => None,
            };
            continue;
        }
        let trimmed = line.trim_start();
        let version = trimmed
            .strip_prefix("version ")
            .or_else(|| trimmed.strip_prefix("version: "));
        if let (Some(n), Some(v)) = (name.take(), version) {
            out.push((n, v.trim().trim_matches('"').to_owned()));
        }
    }
    out
}

/// A package whose `source` is a registry.
fn uv_lock(doc: &toml::Table) -> Vec<(String, String)> {
    toml_packages(doc)
        .filter(|p| {
            p.get("source")
                .and_then(|s| s.as_table())
                .is_some_and(|s| s.contains_key("registry"))
        })
        .filter_map(|p| Some((toml_str(p, "name")?.into(), toml_str(p, "version")?.into())))
        .collect()
}

/// A package with no `source`, or a `legacy` (index) one.
fn poetry_lock(doc: &toml::Table) -> Vec<(String, String)> {
    toml_packages(doc)
        .filter(|p| {
            p.get("source")
                .and_then(|s| s.as_table())
                .and_then(|s| toml_str(s, "type"))
                .is_none_or(|t| t == "legacy")
        })
        .filter_map(|p| Some((toml_str(p, "name")?.into(), toml_str(p, "version")?.into())))
        .collect()
}

/// `name==version` lines, extras and markers dropped; a range, a URL, an
/// option or an include pins nothing.
fn requirements(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.split('#').next()?.split(';').next()?.trim();
            let line = line.split(" --hash").next()?.trim_end_matches('\\').trim();
            let (name, version) = line.split_once("==")?;
            if version.starts_with('=') || line.starts_with('-') || line.contains("://") {
                return None;
            }
            let name = name.split('[').next()?.trim();
            let version = version.split(',').next()?.trim();
            (!version.contains('*')).then(|| (name.to_owned(), version.to_owned()))
        })
        .collect()
}

/// `module version h1:…`, the `/go.mod` lines being the same version's
/// module file.
fn go_sum(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let (module, version) = (parts.next()?, parts.next()?);
            (!version.ends_with("/go.mod")).then(|| (module.to_owned(), version.to_owned()))
        })
        .collect()
}

/// `package:` entries installed by conda (a `pip` one is pypi's).
fn conda_lock(doc: &Value) -> Vec<(String, String)> {
    doc.get("package")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|p| str_of(p, "manager").is_none_or(|m| m == "conda"))
        .filter_map(|p| {
            Some((
                str_of(p, "name")?.to_owned(),
                str_of(p, "version")?.to_owned(),
            ))
        })
        .collect()
}

/// `packages` and `packages-dev`; a `dev-` branch is not a release.
fn composer_lock(doc: &Value) -> Vec<(String, String)> {
    ["packages", "packages-dev"]
        .iter()
        .filter_map(|k| doc.get(k).and_then(Value::as_array))
        .flatten()
        .filter_map(|p| {
            Some((
                str_of(p, "name")?.to_owned(),
                str_of(p, "version")?.to_owned(),
            ))
        })
        .filter(|(_, v)| !v.starts_with("dev-"))
        .collect()
}

/// The `specs:` of a `GEM` section, four spaces in: `name (version)`. The
/// deeper lines are that gem's own requirements.
fn gemfile_lock(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut in_gem = false;
    for line in text.lines() {
        if !line.starts_with(' ') {
            in_gem = line == "GEM";
            continue;
        }
        if !in_gem || !line.starts_with("    ") || line.starts_with("     ") {
            continue;
        }
        let Some((name, rest)) = line.trim().split_once(" (") else {
            continue;
        };
        if let Some(version) = rest.strip_suffix(')') {
            out.push((name.to_owned(), version.to_owned()));
        }
    }
    out
}

/// `dependencies.<framework>.<id>.resolved`; a `Project` reference is not a
/// package.
fn nuget_lock(doc: &Value) -> Vec<(String, String)> {
    doc.get("dependencies")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|fw| fw.values())
        .filter_map(Value::as_object)
        .flatten()
        .filter(|(_, d)| str_of(d, "type") != Some("Project"))
        .filter_map(|(id, d)| Some((id.clone(), str_of(d, "resolved")?.to_owned())))
        .collect()
}

/// `provider "<host>/<namespace>/<type>" {` and its `version = "…"`, named as
/// the registry names a provider: `providers/<namespace>/<type>`.
fn terraform_lock(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("provider \"") {
            current = rest.split('"').next().and_then(|addr| {
                let mut parts = addr.rsplitn(3, '/');
                let (ptype, namespace) = (parts.next()?, parts.next()?);
                Some(format!("providers/{namespace}/{ptype}"))
            });
            continue;
        }
        if let (Some(name), Some(rest)) = (current.as_ref(), line.strip_prefix("version")) {
            let Some(value) = rest.trim_start().strip_prefix('=') else {
                continue;
            };
            out.push((name.clone(), value.trim().trim_matches('"').to_owned()));
            current = None;
        }
    }
    out
}

fn chart_lock(doc: &Value) -> Vec<(String, String)> {
    doc.get("dependencies")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|d| {
            Some((
                str_of(d, "name")?.to_owned(),
                str_of(d, "version")?.to_owned(),
            ))
        })
        .collect()
}

/// `x.y.z` with or without its `v`, as the dist tree spells it (`v20.11.0`);
/// an alias (`lts/*`, `node`) or a partial version pins nothing.
fn nvmrc(text: &str) -> Vec<(String, String)> {
    let v = text.lines().next().unwrap_or("").trim();
    let bare = v.strip_prefix('v').unwrap_or(v);
    let exact = bare.split('.').count() == 3
        && bare
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    if exact {
        vec![(
            crate::services::nodedist::NODEDIST_PACKAGE.to_owned(),
            format!("v{bare}"),
        )]
    } else {
        Vec::new()
    }
}

/// `candidate=version` lines.
fn sdkmanrc(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.split('#').next()?.trim();
            let (c, v) = line.split_once('=')?;
            Some((c.trim().to_owned(), v.trim().to_owned()))
        })
        .collect()
}

/// `[toolchain] channel = "1.80.0"`; `stable` or a dated nightly pins no
/// release.
fn rust_toolchain(doc: &toml::Table) -> Vec<(String, String)> {
    let channel = doc
        .get("toolchain")
        .and_then(|t| t.as_table())
        .and_then(|t| toml_str(t, "channel"))
        .unwrap_or("");
    let exact = channel.split('.').count() == 3
        && channel
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    if exact {
        vec![(
            crate::services::rustup::RUST_PACKAGE.to_owned(),
            channel.to_owned(),
        )]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests;
