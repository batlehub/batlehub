//! `batlehub-cli java …` — the Java engine of batlehub-vsx (its RFC 0002):
//! inspections, fixes, generators and renames over a real Java language
//! server, with no editor, for CI and for agents (`java mcp`).
//!
//! The engine is Node. It is embedded here at build time — the two files
//! batlehub-vsx's `task engine:sync` copies into `assets/java-engine/` — and
//! written to the user's cache on first use, so `batlehub-cli java` needs
//! nothing but a `node` ≥ 22 on `PATH`. `BATLEHUB_JAVA_ENGINE` runs another
//! `batlehub-java.mjs` instead (a checkout's own build, a pinned copy).
//! Every argument after `java` is the engine's: `batlehub-cli java --help`
//! is its help, and its exit code is ours.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::Args;
use sha2::{Digest, Sha256};

const ENGINE: &[u8] = include_bytes!("../../assets/java-engine/batlehub-java.mjs");
const JDT_BUNDLE: &[u8] = include_bytes!("../../assets/java-engine/batlehub-jdt-core.jar");
/// The bundle's `target`, and the first Node with JSON import attributes.
const NODE_MIN: u32 = 22;

#[derive(Args)]
#[command(disable_help_flag = true)]
pub struct JavaArgs {
    /// The engine's verb and its arguments; `batlehub-cli java --help` lists them.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<OsString>,
}

pub fn run(args: JavaArgs) -> Result<()> {
    let engine = match std::env::var_os("BATLEHUB_JAVA_ENGINE") {
        Some(p) => PathBuf::from(p),
        None => {
            let cache = dirs::cache_dir().context("no cache directory for the Java engine")?;
            materialize(&cache.join("batlehub").join("java-engine"))?
        }
    };
    let node = find_on_path("node").with_context(|| {
        format!("batlehub-cli java needs node ≥ {NODE_MIN} on PATH: mise use node@24")
    })?;
    let major = node_major(&node)?;
    if major < NODE_MIN {
        bail!(
            "batlehub-cli java needs node ≥ {NODE_MIN}, {} is {major}: mise use node@24",
            node.display()
        );
    }
    exec(Command::new(node).arg(engine).args(args.args))
}

// ponytail: each CLI version leaves its ~150 KB directory behind; prune the
// siblings when someone notices the cache, not while an older CLI may run one.
/// The embedded engine under `<base>/<content hash>/`, written when absent and
/// rewritten when a file there differs from the embedded bytes. Returns the
/// `.mjs`; the engine finds the jar beside itself.
fn materialize(base: &Path) -> Result<PathBuf> {
    let mut h = Sha256::new();
    h.update(ENGINE);
    h.update(JDT_BUNDLE);
    let dir = base.join(&hex::encode(h.finalize())[..16]);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    for (name, bytes) in [
        ("batlehub-java.mjs", ENGINE),
        ("batlehub-jdt-core.jar", JDT_BUNDLE),
    ] {
        let file = dir.join(name);
        if std::fs::read(&file).ok().as_deref() == Some(bytes) {
            continue;
        }
        // Written beside, then renamed: a concurrent run never reads half a file.
        let part = dir.join(format!("{name}.{}.part", std::process::id()));
        std::fs::write(&part, bytes).with_context(|| format!("writing {}", part.display()))?;
        std::fs::rename(&part, &file).with_context(|| format!("writing {}", file.display()))?;
    }
    Ok(dir.join("batlehub-java.mjs"))
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(&exe))
        .find(|p| p.is_file())
}

fn node_major(node: &Path) -> Result<u32> {
    let out = Command::new(node)
        .arg("--version")
        .output()
        .with_context(|| format!("running {} --version", node.display()))?;
    parse_node_major(&String::from_utf8_lossy(&out.stdout))
        .with_context(|| format!("{} --version printed no version", node.display()))
}

/// `v24.21.0` → 24.
fn parse_node_major(version: &str) -> Option<u32> {
    version
        .trim()
        .strip_prefix('v')?
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// Replaces this process on Unix: stdin and stdout stay the engine's own,
/// which `java mcp` speaks over, and a signal reaches it directly.
#[cfg(unix)]
fn exec(cmd: &mut Command) -> Result<()> {
    use std::os::unix::process::CommandExt;
    Err(cmd.exec()).context("starting node")
}

#[cfg(not(unix))]
fn exec(cmd: &mut Command) -> Result<()> {
    let status = cmd.status().context("starting node")?;
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_embedded_engine_once_and_repairs_a_changed_copy() {
        let base = tempfile::tempdir().unwrap();
        let mjs = materialize(base.path()).unwrap();
        assert_eq!(std::fs::read(&mjs).unwrap(), ENGINE);
        let jar = mjs.with_file_name("batlehub-jdt-core.jar");
        assert_eq!(std::fs::read(&jar).unwrap(), JDT_BUNDLE);
        // Same content, same directory.
        assert_eq!(materialize(base.path()).unwrap(), mjs);
        std::fs::write(&mjs, b"tampered").unwrap();
        materialize(base.path()).unwrap();
        assert_eq!(std::fs::read(&mjs).unwrap(), ENGINE);
        // No .part left behind.
        assert_eq!(std::fs::read_dir(mjs.parent().unwrap()).unwrap().count(), 2);
    }

    #[test]
    fn reads_node_major_versions() {
        assert_eq!(parse_node_major("v24.21.0\n"), Some(24));
        assert_eq!(parse_node_major("v18.0.0"), Some(18));
        assert_eq!(parse_node_major("24.1.0"), None);
        assert_eq!(parse_node_major(""), None);
    }
}
