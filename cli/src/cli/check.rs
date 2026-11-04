//! `batlehub-cli check` (RFC 0040 §4.8): would the pinned dependencies of
//! this project install? Every lockfile `check` can read is read, every pin is
//! asked about through `POST /api/v1/packages/access/batch` (cached metadata
//! only, 50 at a time), and the ones that are not ✅ are listed with their
//! reason. Exit 4 on any ⛔ — and on any ⏳ with `--strict` — so a CI step run
//! before `npm ci` fails naming a dependency, not a `403` deep in npm's log.

use std::path::{Path, PathBuf};

use anyhow::Result;
use batlehub_core::lockfile::{self, Format, Pin, MAX_LOCKFILE_BYTES};
use clap::Args;
use serde::Serialize;
use serde_json::Value;

use crate::api::mise_plan::{host_platform, plan_from_lock, KnownRegistry};
use crate::api::registry::RegistryInfo;
use crate::api::BatleHubClient;
use crate::exit::{CliError, ExitCode};
use crate::render::{ctx, data, warn};

/// What one batch request may carry (decision 37).
const BATCH: usize = 50;

#[derive(Args)]
pub struct CheckArgs {
    /// A lockfile to read; repeatable. Without one, every lockfile `check`
    /// knows in the current directory is read.
    #[arg(long = "lock", value_name = "FILE")]
    pub locks: Vec<PathBuf>,
    /// Fail on a dependency that is not pullable *yet* (⏳) too
    #[arg(long)]
    pub strict: bool,
}

/// One pinned dependency and the server's answer for it.
#[derive(Serialize)]
struct Row {
    lockfile: String,
    coordinate: String,
    /// `pullable`, `waiting`, `refused` or `not_found`.
    state: String,
    reason_code: Option<String>,
    detail: Option<String>,
    next: Option<String>,
}

/// The lockfiles to read: the ones given, else the known ones in `dir`.
fn lockfiles(given: &[PathBuf], dir: &Path) -> Result<Vec<PathBuf>> {
    if !given.is_empty() {
        return Ok(given.to_vec());
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n == "mise.lock" || Format::from_file_name(n).is_some())
        })
        .collect();
    found.sort();
    Ok(found)
}

/// The file's bytes, refused over the cap before they are read (T9).
fn read_capped(path: &Path) -> Result<Vec<u8>> {
    let len = std::fs::metadata(path)
        .map_err(|e| CliError::new(ExitCode::Usage, format!("{}: {e}", path.display())))?
        .len();
    if len > MAX_LOCKFILE_BYTES as u64 {
        return Err(CliError::new(
            ExitCode::Usage,
            format!(
                "{} is {len} bytes, over the {MAX_LOCKFILE_BYTES}-byte limit for a lockfile",
                path.display()
            ),
        )
        .into());
    }
    Ok(std::fs::read(path)?)
}

/// The registry a pin of `kind` is installed from: `-r` when it is that kind,
/// else the first one the caller can install from, a proxy before a local one.
fn registry_for<'a>(kind: &str, registries: &'a [RegistryInfo]) -> Option<&'a RegistryInfo> {
    let of_kind = || registries.iter().filter(move |r| r.registry_type == kind);
    if let Some(r) = ctx().registry.as_deref() {
        if let Some(chosen) = of_kind().find(|x| x.name == r) {
            return Some(chosen);
        }
    }
    let installable = || of_kind().filter(|r| r.installable != Some(false));
    installable()
        .find(|r| r.mode != "local")
        .or_else(|| installable().next())
}

/// `mise.lock`'s pins, through the plan `mise plan` builds: a forge release
/// asset is `owner/repo@tag`. The other tools go through a `generic` mirror
/// (Node.js included: `mise plan` routes nodejs.org there), whose files have
/// no version a registry answers for.
fn mise_pins(text: &str, registries: &[RegistryInfo]) -> Result<Vec<(String, String, String)>> {
    let known: Vec<KnownRegistry> = registries
        .iter()
        .map(|r| KnownRegistry {
            name: r.name.clone(),
            registry_type: r.registry_type.clone(),
        })
        .collect();
    let plan = plan_from_lock("mise.lock", text, &[host_platform()], &known)?;
    let mut pins: Vec<(String, String, String)> = plan
        .entries
        .iter()
        .filter_map(|e| {
            let reg = e.registry.as_ref()?;
            let path = e.url.split_once("://")?.1.split_once('/')?.1;
            let (name, version) = match reg.registry_type.as_str() {
                // `owner/repo/releases/download/<tag>/…`, as GitHub and
                // Forgejo spell a release asset.
                "github" | "forgejo" => {
                    let (repo, rest) = path.split_once("/releases/download/")?;
                    (repo.to_owned(), rest.split('/').next()?.to_owned())
                }
                _ => return None,
            };
            Some((reg.name.clone(), name, version))
        })
        .collect();
    pins.sort();
    pins.dedup();
    Ok(pins)
}

/// The first cause that is not a pass: what a row reports.
fn first_cause(answer: &Value) -> Option<&Value> {
    answer["causes"]
        .as_array()?
        .iter()
        .find(|c| c["state"] == "refuse")
        .or_else(|| {
            answer["causes"]
                .as_array()?
                .iter()
                .find(|c| c["state"] == "wait")
        })
}

pub async fn run(args: CheckArgs, client: &BatleHubClient) -> Result<()> {
    let files = lockfiles(&args.locks, Path::new("."))?;
    if files.is_empty() {
        return Err(
            CliError::new(ExitCode::Usage, "no lockfile here that `check` reads")
                .hint("pass one with --lock <file>")
                .into(),
        );
    }
    // Every file is read, and refused over its cap, before anything is sent.
    let mut contents = Vec::with_capacity(files.len());
    for path in &files {
        contents.push(read_capped(path)?);
    }
    let registries = client.list_registries().await?;

    // (lockfile, registry, name, version)
    let mut asks: Vec<(String, String, String, String)> = Vec::new();
    for (path, bytes) in files.iter().zip(&contents) {
        let file = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned();
        if file == "mise.lock" {
            for (reg, name, version) in mise_pins(&String::from_utf8_lossy(bytes), &registries)? {
                asks.push((file.clone(), reg, name, version));
            }
            continue;
        }
        let Some(format) = Format::from_file_name(&file) else {
            return Err(CliError::new(
                ExitCode::Usage,
                format!("{file}: not a lockfile `check` reads"),
            )
            .into());
        };
        let pins: Vec<Pin> = lockfile::read(format, bytes)
            .map_err(|e| CliError::new(ExitCode::Usage, e.to_string()))?;
        let kind = format.kind().as_str();
        let Some(reg) = registry_for(kind, &registries) else {
            warn(&format!(
                "{file}: no {kind} registry you can install from on this server; its {} pins are not checked",
                pins.len()
            ));
            continue;
        };
        for p in pins {
            asks.push((file.clone(), reg.name.clone(), p.name, p.version));
        }
    }

    let mut rows = Vec::with_capacity(asks.len());
    for chunk in asks.chunks(BATCH) {
        let items: Vec<(String, String, String)> = chunk
            .iter()
            .map(|(_, r, n, v)| (r.clone(), n.clone(), v.clone()))
            .collect();
        let answer = client.access_batch(&items).await?;
        let answers = answer["items"].as_array().cloned().unwrap_or_default();
        for ((file, reg, name, version), a) in chunk.iter().zip(answers) {
            let state = match a["headline"].as_str() {
                Some(h) => h.to_owned(),
                None => "not_found".to_owned(),
            };
            let cause = first_cause(&a);
            rows.push(Row {
                lockfile: file.clone(),
                coordinate: format!("{reg}/{name}@{version}"),
                state,
                reason_code: cause
                    .and_then(|c| c["reason_code"].as_str())
                    .map(str::to_owned),
                detail: cause
                    .and_then(|c| c["detail"].as_str())
                    .map(str::to_owned)
                    .or_else(|| {
                        a.get("state")
                            .map(|_| "no such version here, or one you may not see".to_owned())
                    }),
                next: cause.and_then(|c| c["next"].as_str()).map(str::to_owned),
            });
        }
    }

    let refused = rows
        .iter()
        .filter(|r| r.state == "refused" || r.state == "not_found")
        .count();
    let waiting = rows.iter().filter(|r| r.state == "waiting").count();
    if ctx().structured() {
        data(&rows)?;
    } else {
        print_rows(&rows, refused, waiting);
    }
    if refused > 0 || (args.strict && waiting > 0) {
        return Err(CliError::reported(ExitCode::Refused).into());
    }
    Ok(())
}

fn print_rows(rows: &[Row], refused: usize, waiting: usize) {
    let shown: Vec<&Row> = rows.iter().filter(|r| r.state != "pullable").collect();
    if !shown.is_empty() {
        let mut t = comfy_table::Table::new();
        t.set_header(["", "DEPENDENCY", "LOCKFILE", "WHY", "NEXT"]);
        for r in &shown {
            let mark = match r.state.as_str() {
                "waiting" => "⏳",
                _ => "⛔",
            };
            let why = match (&r.reason_code, &r.detail) {
                (Some(code), Some(d)) => format!("{code}: {d}"),
                (None, Some(d)) => d.clone(),
                (Some(code), None) => code.clone(),
                (None, None) => String::new(),
            };
            t.add_row([
                mark,
                &r.coordinate,
                &r.lockfile,
                &why,
                r.next.as_deref().unwrap_or(""),
            ]);
        }
        println!("{t}");
    }
    let ok = rows.len() - refused - waiting;
    eprintln!(
        "{} dependencies: {ok} pullable, {waiting} waiting, {refused} refused",
        rows.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(name: &str, kind: &str, mode: &str) -> RegistryInfo {
        serde_json::from_value(serde_json::json!({
            "name": name, "type": kind, "mode": mode
        }))
        .unwrap()
    }

    #[test]
    fn a_proxy_registry_is_preferred_to_a_local_one() {
        let regs = [reg("npm-local", "npm", "local"), reg("npm", "npm", "proxy")];
        assert_eq!(registry_for("npm", &regs).unwrap().name, "npm");
        assert!(registry_for("cargo", &regs).is_none());
    }

    #[test]
    fn mise_lock_pins_forge_assets() {
        let lock = r#"
[[tools.ripgrep]]
version = "14.1.0"
backend = "aqua:BurntSushi/ripgrep"
[tools.ripgrep."platforms.linux-x64"]
url = "https://github.com/BurntSushi/ripgrep/releases/download/14.1.0/ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz"
[tools.ripgrep."platforms.macos-arm64"]
url = "https://github.com/BurntSushi/ripgrep/releases/download/14.1.0/ripgrep-14.1.0-aarch64-apple-darwin.tar.gz"
[tools.ripgrep."platforms.windows-x64"]
url = "https://github.com/BurntSushi/ripgrep/releases/download/14.1.0/ripgrep-14.1.0-x86_64-pc-windows-msvc.zip"

"#;
        let regs = [reg("gh", "github", "proxy")];
        assert_eq!(
            mise_pins(lock, &regs).unwrap(),
            vec![("gh".into(), "BurntSushi/ripgrep".into(), "14.1.0".into())]
        );
    }
}
