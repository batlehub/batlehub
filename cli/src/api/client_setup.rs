//! `setup apply | status | remove | env` — writing each package manager's
//! user-level configuration from the server's registry list (RFC 0038).
//!
//! One function, [`plan`], decides everything; the verbs only print or write
//! what it returns. The invariant the module keeps: **nothing outside a marker
//! pair is ever modified** — not even the exact lines `apply` would write,
//! found bare ([`find_run`]): they read as up to date and stay the user's.
//! [`splice`]
//! and [`unsplice`] are the only code that edits a file's content, they copy
//! the bytes around the managed span untouched, and the start marker records
//! the one byte (`nl`) a splice had to add — so `apply` then `remove` gives
//! back the original file, byte for byte.
//!
//! There is no state file: the marker line carries the registries the block
//! was written for (`type:name`, the picks a later run keeps), whether `apply`
//! created the file, Maven's `mirror-of`, and a hash of the body (`sum`) that
//! tells a block someone edited from one that is only stale.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;

use crate::api::registry::RegistryInfo;
use crate::api::suggest::{push_replacement, regex_escape, typed_url_replacements};

// ── Clients ───────────────────────────────────────────────────────────────────

/// The clients of RFC 0038 §4.2, as `--only` / `--skip` take them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ClientId {
    Cargo,
    Npm,
    Pnpm,
    Bun,
    Yarn,
    Pip,
    Uv,
    Go,
    Maven,
    Mise,
    Rustup,
    Node,
}

impl ClientId {
    pub const ALL: [ClientId; 12] = [
        ClientId::Cargo,
        ClientId::Npm,
        ClientId::Pnpm,
        ClientId::Bun,
        ClientId::Yarn,
        ClientId::Pip,
        ClientId::Uv,
        ClientId::Go,
        ClientId::Maven,
        ClientId::Mise,
        ClientId::Rustup,
        ClientId::Node,
    ];

    /// The clients configured only through their environment (`env.sh`).
    pub const ENV_ONLY: [ClientId; 2] = [ClientId::Rustup, ClientId::Node];

    pub fn as_str(self) -> &'static str {
        match self {
            ClientId::Cargo => "cargo",
            ClientId::Npm => "npm",
            ClientId::Pnpm => "pnpm",
            ClientId::Bun => "bun",
            ClientId::Yarn => "yarn",
            ClientId::Pip => "pip",
            ClientId::Uv => "uv",
            ClientId::Go => "go",
            ClientId::Maven => "maven",
            ClientId::Mise => "mise",
            ClientId::Rustup => "rustup",
            ClientId::Node => "node",
        }
    }

    /// The executables whose presence on `PATH` means the client is installed.
    /// `node` has none: `NODEJS_ORG_MIRROR` is read by nvm, fnm, n and
    /// node-gyp, any of which may be installed later.
    fn binaries(self) -> &'static [&'static str] {
        match self {
            ClientId::Cargo => &["cargo"],
            ClientId::Npm => &["npm"],
            ClientId::Pnpm => &["pnpm"],
            ClientId::Bun => &["bun"],
            ClientId::Yarn => &["yarn"],
            ClientId::Pip => &["pip", "pip3"],
            ClientId::Uv => &["uv"],
            ClientId::Go => &["go"],
            ClientId::Maven => &["mvn"],
            ClientId::Mise => &["mise"],
            ClientId::Rustup => &["rustup"],
            ClientId::Node => &[],
        }
    }
}

/// Whether `--only` / `--skip` let a verb touch a file shared by `clients`:
/// one of them is selected and **none** is skipped — `--skip npm` must leave
/// `~/.npmrc` alone even though pnpm reads it too.
fn touches(only: &[ClientId], skip: &[ClientId], clients: &[ClientId]) -> bool {
    clients.iter().any(|c| only.is_empty() || only.contains(c))
        && !clients.iter().any(|c| skip.contains(c))
}

// ── The machine ───────────────────────────────────────────────────────────────

/// Everything [`plan`] reads that is not a file: built from the process by
/// [`Env::from_process`], and by hand in tests so they run against a temp dir.
#[derive(Debug, Clone, Default)]
pub struct Env {
    pub home: PathBuf,
    /// `$XDG_CONFIG_HOME`, else `~/.config`.
    pub config_dir: PathBuf,
    /// `$CARGO_HOME`, else `~/.cargo`.
    pub cargo_home: PathBuf,
    /// The file `go env -w` writes (see [`go_env_path`]); `None` for
    /// `GOENV=off`.
    pub goenv: Option<PathBuf>,
    /// The directory pip reads its user `pip.conf` from (see [`pip_dir`]).
    pub pip_dir: PathBuf,
    /// Basename of `$SHELL`.
    pub shell: Option<String>,
    /// The process environment, for the overrides of RFC 0038 §4.3.
    pub vars: BTreeMap<String, String>,
    pub installed: BTreeSet<ClientId>,
    /// Major version of `yarn`, when it is installed: 1 reads `.npmrc`,
    /// 2 and later only `.yarnrc.yml`.
    pub yarn_major: Option<u32>,
}

impl Env {
    /// `probe_yarn: false` skips `yarn --version` and treats yarn as absent:
    /// `setup env` runs at every shell start and plans no yarn target.
    pub fn from_process(probe_yarn: bool) -> Result<Self> {
        // Phase 1 knows the Unix file layout only (RFC 0038 §11, resolved 4):
        // on Windows every path below would be the wrong file.
        if cfg!(not(unix)) {
            bail!(
                "setup apply/status/remove/env support Linux and macOS only; \
                 use `setup detect` for the snippets on Windows"
            );
        }
        let home =
            dirs::home_dir().ok_or_else(|| anyhow!("cannot determine the home directory"))?;
        // `std::env::vars` panics on a variable that is not UTF-8; such a
        // variable names nothing read here, so it is skipped instead.
        let vars: BTreeMap<String, String> = std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect();
        let non_empty = |k: &str| vars.get(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        // A relative XDG_CONFIG_HOME is invalid (the XDG spec says to ignore
        // it): taken as is, the rc block would source an env.sh relative to
        // whatever directory a shell starts in.
        let config_dir =
            absolute_var(&vars, "XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        let cargo_home = non_empty("CARGO_HOME").unwrap_or_else(|| home.join(".cargo"));
        let path_var = vars.get("PATH").cloned().unwrap_or_default();

        let mut installed: BTreeSet<ClientId> = ClientId::ALL
            .into_iter()
            .filter(|c| c.binaries().iter().any(|b| on_path(b, &path_var)))
            .collect();
        installed.insert(ClientId::Node);

        // corepack's shim prompts to download yarn, or fetches it, on first
        // call: in a hook that is a hang. A yarn that cannot say its version
        // without the network is treated as absent.
        let yarn_major = if probe_yarn && installed.contains(&ClientId::Yarn) {
            let mut cmd = Command::new("yarn");
            cmd.arg("--version")
                .current_dir(&home)
                .env("COREPACK_ENABLE_DOWNLOAD_PROMPT", "0")
                .env("COREPACK_ENABLE_NETWORK", "0");
            output_within(&mut cmd, Duration::from_secs(2))
                .and_then(|v| v.trim().split('.').next()?.parse().ok())
        } else {
            None
        };
        if yarn_major.is_none() {
            installed.remove(&ClientId::Yarn);
        }

        Ok(Self {
            pip_dir: pip_dir(&home, &config_dir),
            goenv: go_env_path(&home, &config_dir, &vars),
            shell: vars
                .get("SHELL")
                .filter(|s| !s.is_empty())
                .cloned()
                .or_else(|| passwd_shell(&home))
                .and_then(|s| {
                    Path::new(&s)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                }),
            home,
            config_dir,
            cargo_home,
            vars,
            installed,
            yarn_major,
        })
    }
}

/// The login shell `/etc/passwd` gives the account whose home is `home`:
/// hooks often run with `$SHELL` unset, and the rc block would silently not be
/// written.
fn passwd_shell(home: &Path) -> Option<String> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        match f.as_slice() {
            [_, _, _, _, _, dir, shell] if Path::new(dir) == home && !shell.is_empty() => {
                Some((*shell).to_owned())
            }
            _ => None,
        }
    })
}

/// pip's user config directory. On macOS pip reads
/// `~/Library/Application Support/pip` *instead of* `~/.config/pip` when that
/// directory exists, so writing the other one would be ignored; its fallback
/// is that literal `~/.config/pip`, which `$XDG_CONFIG_HOME` does not move.
fn pip_dir(home: &Path, config_dir: &Path) -> PathBuf {
    let library = home.join("Library/Application Support/pip");
    match cfg!(target_os = "macos") {
        true if library.is_dir() => library,
        true => home.join(".config/pip"),
        false => config_dir.join("pip"),
    }
}

/// The file `go env -w` writes: `$GOENV`, else `os.UserConfigDir()/go/env`.
/// Computed rather than asked of `go`: `go env` run in a module that wants a
/// newer toolchain downloads that toolchain first, and the answer does not
/// depend on whether go is installed — `remove` must find the file either way.
fn go_env_path(home: &Path, config_dir: &Path, vars: &BTreeMap<String, String>) -> Option<PathBuf> {
    match vars.get("GOENV").map(String::as_str) {
        Some("off") => None,
        Some(p) if Path::new(p).is_absolute() => Some(PathBuf::from(p)),
        _ if cfg!(target_os = "macos") => Some(home.join("Library/Application Support/go/env")),
        _ => Some(config_dir.join("go/env")),
    }
}

fn on_path(binary: &str, path_var: &str) -> bool {
    std::env::split_paths(path_var).any(|dir| is_executable(&dir.join(binary)))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// stdout of `cmd` when it exits 0 within `timeout`; `None` otherwise, with
/// the child killed. The outputs read here are a line long, so the pipe never
/// fills before the child exits.
fn output_within(cmd: &mut Command, timeout: Duration) -> Option<String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let mut out = String::new();
                child.stdout.take()?.read_to_string(&mut out).ok()?;
                return Some(out);
            }
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() >= deadline => {
                // Best effort: a child that already exited cannot be killed,
                // and either way there is no answer to report.
                child.kill().ok();
                child.wait().ok();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

// ── Targets ───────────────────────────────────────────────────────────────────

/// Every file `apply` may write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetId {
    Cargo,
    Npmrc,
    Yarnrc,
    Pip,
    Uv,
    Go,
    Maven,
    Mise,
    EnvSh,
    Fish,
    ShellRc,
}

/// A delimited block inside a file the user owns, or a file managed whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Block,
    DropIn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Comment {
    Hash,
    Xml,
}

// ASCII only: a block spliced into a file that is not UTF-8 (a latin-1
// `~/.bashrc`) is written back byte for byte, which only ASCII survives.
const MARKER_START: &str = ">>> batlehub setup (managed - edit with `batlehub-cli setup`)";
const MARKER_END: &str = "<<< batlehub setup <<<";

impl Comment {
    fn wrap(self, text: &str) -> String {
        match self {
            Comment::Hash => format!("# {text}"),
            Comment::Xml => format!("<!-- {text} -->"),
        }
    }

    /// The marker text inside a comment line, if `line` is one of ours.
    fn unwrap(self, line: &str) -> Option<&str> {
        let line = line.trim_end_matches(['\r', '\n']);
        match self {
            Comment::Hash => line.strip_prefix("# "),
            Comment::Xml => line.strip_prefix("<!-- ")?.strip_suffix(" -->"),
        }
    }
}

struct Target {
    path: PathBuf,
    shape: Shape,
    comment: Comment,
}

impl TargetId {
    fn target(self, env: &Env) -> Option<Target> {
        let (path, shape, comment) = match self {
            TargetId::Cargo => (
                env.cargo_home.join("config.toml"),
                Shape::Block,
                Comment::Hash,
            ),
            TargetId::Npmrc => (env.home.join(".npmrc"), Shape::Block, Comment::Hash),
            TargetId::Yarnrc => (env.home.join(".yarnrc.yml"), Shape::Block, Comment::Hash),
            TargetId::Pip => (env.pip_dir.join("pip.conf"), Shape::Block, Comment::Hash),
            TargetId::Uv => (
                env.config_dir.join("uv/uv.toml"),
                Shape::Block,
                Comment::Hash,
            ),
            TargetId::Go => (env.goenv.clone()?, Shape::Block, Comment::Hash),
            TargetId::Maven => (
                env.home.join(".m2/settings.xml"),
                Shape::DropIn,
                Comment::Xml,
            ),
            TargetId::Mise => (
                mise_dir(env).join("conf.d/batlehub.toml"),
                Shape::DropIn,
                Comment::Hash,
            ),
            TargetId::EnvSh => (env_sh_path(env), Shape::DropIn, Comment::Hash),
            TargetId::Fish => (
                env.config_dir.join("fish/conf.d/batlehub.fish"),
                Shape::DropIn,
                Comment::Hash,
            ),
            TargetId::ShellRc => (rc_path(env)?, Shape::Block, Comment::Hash),
        };
        Some(Target {
            path,
            shape,
            comment,
        })
    }

    /// The clients this file configures, before installation is considered.
    fn clients(self, env: &Env) -> Vec<ClientId> {
        let yarn_reads_npmrc = env.yarn_major.is_some_and(|m| m < 2);
        match self {
            TargetId::Cargo => vec![ClientId::Cargo],
            TargetId::Npmrc => {
                let mut c = vec![ClientId::Npm, ClientId::Pnpm, ClientId::Bun];
                if yarn_reads_npmrc {
                    c.push(ClientId::Yarn);
                }
                c
            }
            // Kept under yarn 1 too: the yarn 2+ a project pins through
            // corepack reads ~/.yarnrc.yml, which yarn 1 ignores — and a
            // block already there must stay managed, not go stale.
            TargetId::Yarnrc => vec![ClientId::Yarn],
            TargetId::Pip => vec![ClientId::Pip],
            TargetId::Uv => vec![ClientId::Uv],
            TargetId::Go => vec![ClientId::Go],
            TargetId::Maven => vec![ClientId::Maven],
            TargetId::Mise => vec![ClientId::Mise],
            TargetId::EnvSh | TargetId::Fish | TargetId::ShellRc => ClientId::ENV_ONLY.to_vec(),
        }
    }

    /// Whether the file is TOML, so the spliced result can be parsed before
    /// it is written.
    fn is_toml(self) -> bool {
        matches!(self, TargetId::Cargo | TargetId::Uv | TargetId::Mise)
    }
}

/// The files of `id`: one per target but the rc block, which goes in the login
/// shell's rc file and stays in any other rc file that already carries one —
/// a block written before the user changed shells, or with `$SHELL` unset in a
/// hook, is still managed. `all` takes both rc files whatever they hold, for
/// `remove`.
fn targets(id: TargetId, env: &Env, all: bool) -> Vec<Target> {
    let mut out: Vec<Target> = id.target(env).into_iter().collect();
    if id == TargetId::ShellRc {
        for path in all_rc_paths(env) {
            if !out.iter().any(|t| t.path == path) && (all || has_start_marker(&path)) {
                out.push(Target {
                    path,
                    shape: Shape::Block,
                    comment: Comment::Hash,
                });
            }
        }
    }
    out
}

fn has_start_marker(path: &Path) -> bool {
    read_text(path)
        .ok()
        .flatten()
        .is_some_and(|(c, _)| c.lines().any(|l| is_start_line(Comment::Hash, l)))
}

fn env_sh_path(env: &Env) -> PathBuf {
    env.config_dir.join("batlehub/env.sh")
}

/// The rc file `--shell-rc auto` writes for the login shell: `~/.bashrc` is
/// what the non-login interactive terminals of Che and devcontainers read, and
/// `~/.zshenv` is read by every zsh, the non-interactive ones IDE tasks spawn
/// included. fish gets a conf.d drop-in instead.
fn rc_path(env: &Env) -> Option<PathBuf> {
    match env.shell.as_deref()? {
        "bash" => Some(bash_rc(env, cfg!(target_os = "macos"))),
        "zsh" => Some(zshenv(env)),
        _ => None,
    }
}

/// macOS terminals start bash as a login shell, which reads the first of
/// `~/.bash_profile`, `~/.bash_login`, `~/.profile` and never `~/.bashrc`.
/// Creating `.bash_profile` beside an existing `.profile` would hide it, so
/// the one bash already reads gets the block.
fn bash_rc(env: &Env, macos: bool) -> PathBuf {
    if !macos {
        return env.home.join(".bashrc");
    }
    [".bash_profile", ".bash_login", ".profile"]
        .iter()
        .map(|f| env.home.join(f))
        .find(|p| p.exists())
        .unwrap_or_else(|| env.home.join(".bash_profile"))
}

/// zsh reads `$ZDOTDIR/.zshenv` when `ZDOTDIR` is set *as it starts*. A
/// `~/.zshenv` that sets `ZDOTDIR` itself (the XDG layout) is read first, by
/// every zsh, so it gets the block — not the directory it points the rest at.
fn zshenv(env: &Env) -> PathBuf {
    let home = env.home.join(".zshenv");
    let sets_zdotdir = read_text(&home).ok().flatten().is_some_and(|(c, _)| {
        c.lines().any(|l| {
            let l = l.trim_start();
            let l = l.strip_prefix("export ").unwrap_or(l);
            l.starts_with("ZDOTDIR=")
        })
    });
    if sets_zdotdir {
        return home;
    }
    absolute_var(&env.vars, "ZDOTDIR").map_or(home, |d| d.join(".zshenv"))
}

/// mise's config directory: `$MISE_CONFIG_DIR`, else `~/.config/mise`.
fn mise_dir(env: &Env) -> PathBuf {
    absolute_var(&env.vars, "MISE_CONFIG_DIR").unwrap_or_else(|| env.config_dir.join("mise"))
}

/// A directory variable a file is written under, only when absolute: a
/// relative one would land the file under the current directory.
fn absolute_var(vars: &BTreeMap<String, String>, name: &str) -> Option<PathBuf> {
    vars.get(name)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// Every rc file a block may have been written to, whatever the shell is now.
fn all_rc_paths(env: &Env) -> Vec<PathBuf> {
    let mut paths = vec![env.home.join(".bashrc"), env.home.join(".zshenv")];
    for p in [zshenv(env), bash_rc(env, cfg!(target_os = "macos"))] {
        if !paths.contains(&p) {
            paths.push(p);
        }
    }
    paths
}

// ── Markers ───────────────────────────────────────────────────────────────────

/// The fields of a start marker — the command's only state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marker {
    /// `(type, name)` of each registry the block was written for; a later run
    /// keeps them as its picks.
    pub registries: Vec<(String, String)>,
    /// `apply` created the file, so `remove` deletes it once empty.
    pub created: bool,
    /// `apply` added a final newline the original lacked.
    pub nl: bool,
    /// The block was put at the top of the file: in an rc file, below a
    /// non-interactive early `return` (the stock `~/.bashrc` has one), an
    /// appended block is never read by `bash -lc` or a login shell.
    pub top: bool,
    /// Maven's `--maven-mirror-of`.
    pub mirror_of: Option<String>,
    /// [`body_sum`] of the body as written. Set by the renderers, never by
    /// hand; absent from a marker that predates it.
    pub sum: Option<String>,
}

impl Marker {
    fn names(&self) -> Vec<String> {
        self.registries.iter().map(|(_, n)| n.clone()).collect()
    }

    /// The name this marker records for `registry_type`.
    fn name_for(&self, registry_type: &str) -> Option<&str> {
        self.registries
            .iter()
            .find(|(t, _)| t == registry_type)
            .map(|(_, n)| n.as_str())
    }

    fn render(&self, comment: Comment) -> String {
        let regs: Vec<String> = self
            .registries
            .iter()
            .map(|(t, n)| format!("{t}:{n}"))
            .collect();
        let mut s = format!("{MARKER_START} registry={}", regs.join(","));
        if self.created {
            s.push_str(" created");
        }
        if self.nl {
            s.push_str(" nl");
        }
        if self.top {
            s.push_str(" top");
        }
        if let Some(m) = &self.mirror_of {
            s.push_str(&format!(" mirror-of={m}"));
        }
        if let Some(sum) = &self.sum {
            s.push_str(&format!(" sum={sum}"));
        }
        s.push_str(" >>>");
        comment.wrap(&s)
    }

    fn parse(comment: Comment, line: &str) -> Option<Self> {
        let fields = comment
            .unwrap(line)?
            .strip_prefix(MARKER_START)?
            .strip_suffix(">>>")?;
        let mut m = Marker::default();
        for field in fields.split_whitespace() {
            match field.split_once('=') {
                Some(("registry", v)) => {
                    m.registries = v
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .filter_map(|e| e.split_once(':'))
                        .map(|(t, n)| (t.to_owned(), n.to_owned()))
                        .collect();
                }
                Some(("mirror-of", v)) => m.mirror_of = Some(v.to_owned()),
                Some(("sum", v)) => m.sum = Some(v.to_owned()),
                _ if field == "created" => m.created = true,
                _ if field == "nl" => m.nl = true,
                _ if field == "top" => m.top = true,
                _ => {}
            }
        }
        Some(m)
    }

    /// Whether `body` is no longer what was written under this marker: edited
    /// by hand, or by the client itself (`go env -w` rewrites its line in
    /// place). A stale body still hashes to the marker's `sum`.
    fn edited(&self, body: &str) -> bool {
        self.sum.as_ref().is_some_and(|s| *s != body_sum(body))
    }
}

/// FNV-1a of `body`, trailing newlines and every `\r` dropped: an editor that
/// rewrote the line endings has not edited a setting. A change detector, not a
/// defence — anyone who can edit the body can edit the marker.
fn body_sum(body: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in body
        .trim_end_matches(['\n', '\r'])
        .bytes()
        .filter(|&b| b != b'\r')
    {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// A managed block found in a file: byte offsets of its first and one-past-last
/// byte (end marker line included, with its newline), and of its body.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Span {
    start: usize,
    body_start: usize,
    body_end: usize,
    end: usize,
    marker: Marker,
}

fn is_start_line(comment: Comment, line: &str) -> bool {
    comment
        .unwrap(line)
        .is_some_and(|t| t.starts_with(MARKER_START))
}

fn is_end_line(comment: Comment, line: &str) -> bool {
    comment.unwrap(line).is_some_and(|t| t == MARKER_END)
}

/// The one managed block in `content`, if any. A start without an end (or the
/// reverse), or two blocks, cannot be replaced safely and is an error.
fn find_block(content: &str, comment: Comment) -> Result<Option<Span>> {
    let mut start: Option<(usize, usize, Marker)> = None;
    let mut found: Option<Span> = None;
    let mut offset = 0;
    for line in content.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        if is_start_line(comment, line) {
            if start.is_some() || found.is_some() {
                bail!("more than one batlehub setup block");
            }
            let marker = Marker::parse(comment, line)
                .ok_or_else(|| anyhow!("unreadable batlehub setup start marker"))?;
            start = Some((line_start, offset, marker));
        } else if is_end_line(comment, line) {
            let (s, body_start, marker) = start
                .take()
                .ok_or_else(|| anyhow!("batlehub setup end marker without a start marker"))?;
            found = Some(Span {
                start: s,
                body_start,
                body_end: line_start,
                end: offset,
                marker,
            });
        }
    }
    if start.is_some() {
        bail!("batlehub setup start marker without an end marker");
    }
    Ok(found)
}

fn render_block(marker: &Marker, comment: Comment, body: &str) -> String {
    let marker = Marker {
        sum: Some(body_sum(body)),
        ..marker.clone()
    };
    let mut s = marker.render(comment);
    s.push('\n');
    s.push_str(body.trim_end_matches('\n'));
    s.push('\n');
    s.push_str(&comment.wrap(MARKER_END));
    s.push('\n');
    s
}

/// `original` with the managed block set to `body`. An existing block is
/// replaced in place and keeps its `created`/`nl`/`top`; a new one is
/// appended after a blank line, or put first when `top`.
fn splice(
    original: Option<&str>,
    comment: Comment,
    mut marker: Marker,
    body: &str,
    top: bool,
) -> Result<String> {
    let content = original.unwrap_or("");
    if let Some(span) = find_block(content, comment)? {
        marker.created = span.marker.created;
        marker.nl = span.marker.nl;
        marker.top = span.marker.top;
        return Ok(format!(
            "{}{}{}",
            &content[..span.start],
            render_block(&marker, comment, body),
            &content[span.end..]
        ));
    }
    marker.created = original.is_none();
    if top {
        marker.top = true;
        marker.nl = false;
        return Ok(format!("{}{content}", render_block(&marker, comment, body)));
    }
    if content.is_empty() {
        marker.nl = false;
        return Ok(render_block(&marker, comment, body));
    }
    let mut out = content.to_owned();
    marker.nl = !out.ends_with('\n');
    if marker.nl {
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&render_block(&marker, comment, body));
    Ok(out)
}

/// `content` with the managed block taken out, with the two bytes `splice`
/// may have added — but only when they are still the ones it added:
///
/// - the blank line before an appended block, only if the line above the
///   block is blank (a user may have written right above it);
/// - the newline added to a file that had none (`nl`), only while the block
///   is still last — with lines after it, taking that newline out would glue
///   the line above the block to the one below.
///
/// `None` when the file should be deleted: `apply` created it and nothing else
/// is left.
fn unsplice(content: &str, comment: Comment) -> Result<Option<String>> {
    let Some(span) = find_block(content, comment)? else {
        return Ok(Some(content.to_owned()));
    };
    let mut prefix = &content[..span.start];
    let suffix = &content[span.end..];
    if span.marker.top {
        // Put first, with nothing added around it.
    } else if prefix.ends_with("\n\n") {
        prefix = prefix.strip_suffix('\n').unwrap_or(prefix);
    }
    if span.marker.nl && suffix.is_empty() {
        prefix = prefix.strip_suffix('\n').unwrap_or(prefix);
    }
    let out = format!("{prefix}{suffix}");
    if out.is_empty() && span.marker.created {
        return Ok(None);
    }
    Ok(Some(out))
}

/// The part of `content` the user owns: everything outside the block.
fn user_part(content: &str, comment: Comment) -> Result<String> {
    Ok(match find_block(content, comment)? {
        Some(span) => format!("{}{}", &content[..span.start], &content[span.end..]),
        None => content.to_owned(),
    })
}

/// The marker of a drop-in, which is its first line; `None` for a file the
/// user wrote under the same name.
fn drop_in_marker(content: &str, comment: Comment) -> Option<Marker> {
    let first = content.lines().next()?;
    Marker::parse(comment, first)
}

/// A drop-in's body: everything after its marker line.
fn drop_in_body(content: &str) -> &str {
    content.split_once('\n').map_or("", |(_, rest)| rest)
}

fn render_drop_in(marker: &Marker, comment: Comment, body: &str) -> String {
    let marker = Marker {
        sum: Some(body_sum(body)),
        ..marker.clone()
    };
    format!(
        "{}\n{}\n",
        marker.render(comment),
        body.trim_end_matches('\n')
    )
}

/// The byte span of `body`'s lines, found bare in `content`: the block
/// `apply` wrote, after a client rewrote its file without comments — npm on
/// `npm config set` and `npm login`, pip on `pip config set`, yarn on
/// `yarn config set`. Only the exact lines `apply` would write now count, so a
/// `registry=` the user set themselves — even to another registry of the same
/// server — is never taken. A body that opens an ini section must still be the
/// whole section: a key the client added under it makes the run not ours.
fn find_run(content: &str, body: &str) -> Option<(usize, usize)> {
    let want: Vec<&str> = body.trim_end_matches('\n').lines().collect();
    let first = *want.first()?;
    // (start, end, line); the running sum never exceeds `content.len()`.
    let lines: Vec<(usize, usize, &str)> = content
        .split_inclusive('\n')
        .scan(0, |offset, line| {
            let start = *offset;
            *offset += line.len();
            Some((start, *offset, line))
        })
        .collect();
    fn bare(l: &str) -> &str {
        l.trim_end_matches(['\n', '\r']).trim_end()
    }
    (0..lines.len()).find_map(|i| {
        let (run, rest) = lines.get(i..)?.split_at_checked(want.len())?;
        if !run.iter().zip(&want).all(|((_, _, l), w)| bare(l) == *w) {
            return None;
        }
        // A YAML list or mapping continued on the next line belongs to the
        // run's last key.
        if rest
            .first()
            .is_some_and(|(_, _, l)| l.starts_with([' ', '\t']) || l.starts_with("- "))
        {
            return None;
        }
        // configparser keeps reading the section past blank lines, up to the
        // next header.
        if first.starts_with('[')
            && rest
                .iter()
                .map(|(_, _, l)| bare(l))
                .take_while(|l| !l.starts_with('['))
                .any(|l| !l.is_empty() && !l.starts_with(['#', ';']))
        {
            return None;
        }
        let (start, _, _) = run.first()?;
        let (_, end, _) = run.last()?;
        Some((*start, *end))
    })
}

fn without(content: &str, (start, end): (usize, usize)) -> String {
    format!("{}{}", &content[..start], &content[end..])
}

// ── Registries ────────────────────────────────────────────────────────────────

/// `--pick type=name`, parsed.
pub type RegistryPicks = BTreeMap<String, String>;

pub fn parse_picks(raw: &[String]) -> Result<RegistryPicks> {
    raw.iter()
        .map(|p| {
            p.split_once('=')
                .filter(|(t, n)| !t.is_empty() && !n.is_empty())
                .map(|(t, n)| (t.to_owned(), n.to_owned()))
                .ok_or_else(|| anyhow!("--pick takes <type>=<name>, got {p:?}"))
        })
        .collect()
}

/// A registry can stand in for a public upstream only in `proxy` or `hybrid`
/// mode: a `local` one serves only what was published to it, and pointing
/// `~/.npmrc` at it breaks every public install. Nor one the server says the
/// caller cannot install from (listed for its metadata alone, say): the
/// client would get a 401 on every download (RFC 0038 §4.4).
pub(crate) fn is_candidate(r: &RegistryInfo) -> bool {
    matches!(r.mode.as_str(), "proxy" | "hybrid") && r.installable != Some(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Resolution<'a> {
    One(&'a RegistryInfo),
    None,
    Ambiguous(Vec<String>),
}

/// The registry of `registry_type` a client should use, and a note when the
/// choice changed under the user: the `--pick`; else the one the
/// target's own marker records (`own`); else the only candidate; else the one
/// candidate a marker elsewhere on this machine names.
///
/// When the registry `own` names is gone, the block is not silently re-pointed:
/// one remaining candidate is taken with a note; none or several leave
/// nothing chosen and the block `orphaned` — kept on its dead URL, so the
/// client fails closed instead of going back to the public upstream — with a
/// note saying what to do.
fn resolve<'a>(
    registry_type: &str,
    registries: &'a [RegistryInfo],
    picks: &RegistryPicks,
    own: Option<&str>,
    marked: &BTreeSet<(String, String)>,
) -> Result<(Resolution<'a>, Option<String>)> {
    let candidates: Vec<&RegistryInfo> = registries
        .iter()
        .filter(|r| r.registry_type == registry_type && is_candidate(r))
        .collect();
    if let Some(name) = picks.get(registry_type) {
        return candidates
            .iter()
            .find(|r| &r.name == name)
            .map(|r| (Resolution::One(r), None))
            .ok_or_else(|| {
                anyhow!("--pick {registry_type}={name}: the server lists no proxy or hybrid {registry_type} registry of that name")
            });
    }
    if let Some(old) = own {
        if let Some(r) = candidates.iter().find(|r| r.name == old) {
            return Ok((Resolution::One(r), None));
        }
        return Ok(match candidates.as_slice() {
            [] => (
                Resolution::None,
                Some(format!(
                    "{registry_type} registry {old} is no longer listed; `setup remove` to stop using the instance"
                )),
            ),
            [one] => (
                Resolution::One(one),
                Some(format!(
                    "{registry_type} registry {old} is gone; switched to {}",
                    one.name
                )),
            ),
            many => (
                Resolution::None,
                Some(format!(
                    "{registry_type} registry {old} is gone; pick one of {} with --pick {registry_type}=<name>",
                    many.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", ")
                )),
            ),
        });
    }
    Ok(match candidates.as_slice() {
        [] => (Resolution::None, None),
        [one] => (Resolution::One(one), None),
        many => {
            let named: Vec<&&RegistryInfo> = many
                .iter()
                .filter(|r| marked.contains(&(registry_type.to_owned(), r.name.clone())))
                .collect();
            match named.as_slice() {
                [one] => (Resolution::One(one), None),
                _ => (
                    Resolution::Ambiguous(many.iter().map(|r| r.name.clone()).collect()),
                    None,
                ),
            }
        }
    })
}

/// Host of `url`, when it parses.
fn url_host(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()?
        .host_str()
        .map(str::to_ascii_lowercase)
}

/// The one shape of URL setup writes into a file or prints for `eval`: ASCII,
/// no control, space or quoting character, scheme `http` or `https`, no
/// credentials, query or fragment. The string itself is checked, not only what
/// a URL parser makes of it — a parser drops a `\n` that `~/.npmrc` would read
/// as a second setting.
fn is_plain_http_url(u: &str) -> bool {
    u.is_ascii()
        && !u
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || UNSAFE.contains(c))
        && reqwest::Url::parse(u).is_ok_and(|p| {
            matches!(p.scheme(), "http" | "https")
                && p.username().is_empty()
                && p.password().is_none()
                && p.query().is_none()
                && p.fragment().is_none()
        })
}

/// A generic registry's upstream, as it goes into a rule: a plain http(s) URL
/// with nothing a TOML key or a regex could misread, without its final `/`.
fn check_upstream(u: &str) -> std::result::Result<&str, String> {
    if is_plain_http_url(u) {
        Ok(u.trim_end_matches('/'))
    } else {
        Err(format!("its upstream {u:?} is not a plain http(s) URL"))
    }
}

/// Why `r`'s mise rules must not be written: they rewrite a fixed public host
/// (`codeberg.org`, `api.github.com`, …) onto the registry, which is only
/// right when the registry proxies that host. A forgejo registry in front of
/// `git.corp.example` would otherwise take over every codeberg.org download.
/// An unknown upstream (an older server) is not assumed public.
fn foreign_upstream(ty: &str, r: &RegistryInfo) -> Option<String> {
    let public = match ty {
        "github" => "api.github.com",
        "forgejo" => "codeberg.org",
        "npm" => "registry.npmjs.org",
        "cargo" => "crates.io",
        "openvsx" => "open-vsx.org",
        _ => return None,
    };
    match r.upstream.as_deref().and_then(url_host) {
        Some(h) if h == public => None,
        Some(h) => Some(format!(
            "{ty} registry {} (it proxies {h}, not {public})",
            r.name
        )),
        None => Some(format!(
            "{ty} registry {} (the server does not say what it proxies: that needs \
             `upstreams:read` granted to `*` on it)",
            r.name
        )),
    }
}

/// `url` is a registry URL of the instance at `server`: same scheme and port,
/// on its host under `{server path}/proxy/` (a server reached under a path
/// prefix), or on one of its subdomains (a registry's own host). Another
/// service on the same host — a verdaccio on :4873 — is not.
fn is_instance_url(url: &str, server: &str) -> bool {
    let (Ok(u), Ok(s)) = (reqwest::Url::parse(url), reqwest::Url::parse(server)) else {
        return false;
    };
    let (Some(h), Some(sh)) = (u.host_str(), s.host_str()) else {
        return false;
    };
    let (h, sh) = (h.to_ascii_lowercase(), sh.to_ascii_lowercase());
    let proxy = format!("{}/proxy/", s.path().trim_end_matches('/'));
    u.scheme() == s.scheme()
        && u.port_or_known_default() == s.port_or_known_default()
        && ((h == sh && u.path().starts_with(&proxy)) || h.ends_with(&format!(".{sh}")))
}

/// `host` is `server_host` or one of its subdomains.
fn on_host(host: &str, server_host: &str) -> bool {
    host == server_host || host.ends_with(&format!(".{server_host}"))
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
}

/// A registry name safe to write into every target unquoted: in a URL path,
/// a TOML or XML value, a marker field. The server does not constrain names;
/// a name outside this set is refused rather than escaped per format.
fn check_name(name: &str) -> Result<()> {
    // `.` and `..` would climb out of `/proxy/` in the written URL.
    if name.chars().all(|c| c == '.')
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        bail!("registry name {name:?} has characters setup will not write into a config file");
    }
    Ok(())
}

/// The URL clients are pointed at, after the checks of RFC 0038 §4.5. The URL
/// is written raw into ini, TOML, XML and shell files, so the string itself is
/// checked, not only what a URL parser makes of it — a parser drops a `\n`
/// that `~/.npmrc` would read as a second setting:
///
/// - no control, space or quoting character, no credentials, query or
///   fragment, scheme `http` or `https`;
/// - a `public_url` on the server's host or one of its subdomains, unless
///   `--allow-foreign-hosts`;
/// - plain `http` only for loopback or under `--insecure-http`, as for the
///   server itself.
fn base_url(r: &RegistryInfo, opts: &Options) -> Result<String> {
    check_name(&r.name)?;
    let base = r.base_url(opts.server);
    if !is_plain_http_url(&base) {
        bail!(
            "registry {} advertises {base:?}: only a plain http(s) URL — ASCII, without spaces, \
             quoting characters, credentials, query or fragment — is written",
            r.name
        );
    }
    let url = reqwest::Url::parse(&base)?;
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let server = reqwest::Url::parse(opts.server)?;
    let server_host = server.host_str().unwrap_or_default().to_ascii_lowercase();
    if !opts.allow_foreign_hosts
        && (!on_host(&host, &server_host)
            // Explicit ports: an `http` public_url on an `https` server
            // (allowed by --insecure-http) changes the default one.
            || url.port() != server.port())
    {
        bail!(
            "registry {} advertises {base}, which is not on {server_host} (same port); \
             pass --allow-foreign-hosts to write it anyway",
            r.name
        );
    }
    if url.scheme() == "http" && !is_loopback(&host) && !opts.insecure_http {
        bail!(
            "registry {} advertises {base}, which is plain HTTP; pass --insecure-http to write it anyway",
            r.name
        );
    }
    // What was checked is what is written: the parser's form, not the raw
    // string a client's own parser might read differently (`0x7f.0.0.1`,
    // `HTTP:`, `a/../b`).
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// Characters no URL `apply` writes may hold: quoting and substitution for
/// the shell, TOML, YAML and XML; `,` and `|`, which separate proxies in
/// GOPROXY (a URL parser reads `https://hub/p,https://evil` as one URL on
/// `hub`, go as two).
const UNSAFE: &str = "\"'\\<>`$;|&{}^,";

/// Refuse a plain-HTTP server outside loopback: the registry list is the only
/// thing `apply` trusts, and over HTTP anyone on the path rewrites it.
pub fn check_server_scheme(server: &str, insecure_http: bool) -> Result<()> {
    if server.chars().any(|c| c.is_control() || c.is_whitespace()) {
        bail!("invalid server URL {server:?}");
    }
    let url =
        reqwest::Url::parse(server).with_context(|| format!("invalid server URL {server:?}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("{server} is not an http(s) URL");
    }
    let loopback = url.host_str().is_some_and(is_loopback);
    if url.scheme() == "http" && !loopback && !insecure_http {
        bail!(
            "{server} is plain HTTP: anyone on the path could redirect every install. \
             Use https://, or pass --insecure-http"
        );
    }
    Ok(())
}

// ── Plan ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    NotInstalled,
    NoRegistry,
    Ambiguous,
    Absent,
    UpToDate,
    Drifted,
    Orphaned,
    Conflict,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::NotInstalled => "not-installed",
            State::NoRegistry => "no-registry",
            State::Ambiguous => "ambiguous",
            State::Absent => "absent",
            State::UpToDate => "up-to-date",
            State::Drifted => "drifted",
            State::Orphaned => "orphaned",
            State::Conflict => "conflict",
        }
    }

    /// Whether `status` reports this as settled (exit 0) or as something to do
    /// or look at (exit 3).
    pub fn settled(self) -> bool {
        matches!(
            self,
            State::NotInstalled | State::NoRegistry | State::UpToDate
        )
    }
}

/// `--maven-mirror-of`, named so a shell never globs it: the `<mirrorOf>`
/// values themselves (`*`, `external:*`) would expand to file names unquoted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum MavenMirror {
    /// Maven Central only; repositories a POM declares are left alone
    Central,
    /// Every remote repository (`external:*`): all but localhost and file:
    External,
    /// Every repository (`*`)
    All,
}

impl MavenMirror {
    pub fn mirror_of(self) -> &'static str {
        match self {
            MavenMirror::Central => "central",
            MavenMirror::External => "external:*",
            MavenMirror::All => "*",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ShellRc {
    Auto,
    None,
}

pub struct Options<'a> {
    pub server: &'a str,
    pub picks: RegistryPicks,
    pub maven_mirror_of: Option<MavenMirror>,
    /// `None` leaves the rc block and the fish drop-in out of the plan
    /// entirely: neither written nor taken out.
    pub shell_rc: ShellRc,
    pub only: Vec<ClientId>,
    pub skip: Vec<ClientId>,
    pub allow_foreign_hosts: bool,
    pub insecure_http: bool,
}

/// What `apply` would do to one file.
#[derive(Debug, Clone, Serialize)]
pub struct TargetPlan {
    pub target: TargetId,
    #[serde(serialize_with = "lossy_path")]
    pub path: PathBuf,
    pub clients: Vec<ClientId>,
    pub state: State,
    pub registries: Vec<String>,
    /// Why the state is what it is: the conflicting source, the candidates of
    /// an ambiguous type, a registry that went away.
    pub detail: Option<String>,
    /// The snippet to merge by hand, for a conflict.
    pub snippet: Option<String>,
    #[serde(skip)]
    pub current: Option<String>,
    /// The file's content after `apply`; `None` deletes it. Only meaningful for
    /// `absent` and `drifted`.
    #[serde(skip)]
    pub next: Option<String>,
    /// Variables this target exports (env.sh), for `setup env`.
    #[serde(skip)]
    pub env: Vec<(String, String)>,
    /// The file is not UTF-8: `next` is written one byte per char.
    #[serde(skip)]
    pub latin1: bool,
}

impl TargetPlan {
    pub fn writes(&self) -> bool {
        matches!(self.state, State::Absent | State::Drifted)
    }

    fn conflict(&mut self, why: String) {
        self.state = State::Conflict;
        self.detail = Some(why);
    }
}

/// Every `(type, name)` the markers on this machine record, read once so
/// [`resolve`] can keep the pick a sibling target recorded (`.npmrc` and
/// `.yarnrc.yml` share the npm pick) — for that type only.
fn marked_registries(env: &Env) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for id in ALL_TARGETS {
        for t in targets(id, env, false) {
            let Ok(content) = std::fs::read_to_string(&t.path) else {
                continue;
            };
            let marker = match t.shape {
                Shape::DropIn => drop_in_marker(&content, t.comment),
                Shape::Block => find_block(&content, t.comment)
                    .ok()
                    .flatten()
                    .map(|s| s.marker),
            };
            out.extend(marker.into_iter().flat_map(|m| m.registries));
        }
    }
    out
}

const ALL_TARGETS: [TargetId; 11] = [
    TargetId::Cargo,
    TargetId::Npmrc,
    TargetId::Yarnrc,
    TargetId::Pip,
    TargetId::Uv,
    TargetId::Go,
    TargetId::Maven,
    TargetId::Mise,
    TargetId::EnvSh,
    TargetId::Fish,
    TargetId::ShellRc,
];

/// Registry types the mise drop-in carries settings for: the ones whose URLs
/// mise fetches itself, plus `generic`, one rule per upstream the registry
/// list shows (RFC 0038 §6.4) — none when the caller lacks `upstreams:read`.
/// `generic` last: mise stops at the first rule that matches, and a mirror of
/// `https://github.com` must not shadow the typed github rules.
const MISE_TYPES: [&str; 7] = [
    "github", "forgejo", "npm", "cargo", "openvsx", "nodedist", "generic",
];

/// The plan for every file, from the registry list and the machine. Fails only
/// on a `--pick` the server does not list (used or not): everything else that goes
/// wrong with one file is that file's `conflict`, so one unreadable dotfile
/// does not stop every other client from being configured — or `remove` from
/// running.
pub fn plan(registries: &[RegistryInfo], opts: &Options, env: &Env) -> Result<Vec<TargetPlan>> {
    // A pick is checked even when no selected client would use it: a typo
    // must not pass silently because the client it was for is skipped.
    for ty in opts.picks.keys() {
        resolve(ty, registries, &opts.picks, None, &BTreeSet::new())?;
    }
    let mut ctx = PlanCtx {
        registries,
        opts,
        env,
        marked: marked_registries(env),
        rc_user: rc_user_part(env),
        env_out: None,
    };
    let mut out = Vec::new();
    for id in ALL_TARGETS {
        for t in targets(id, env, false) {
            out.extend(ctx.plan_target(id, &t)?);
        }
    }
    Ok(out)
}

/// What `env.sh` resolved, for the fish drop-in and the rc block planned after
/// it: they only carry what `env.sh` does, and neither is written when
/// `env.sh` is not — an rc line sourcing a file that was never written fails
/// every shell that starts.
struct EnvOut {
    vars: Vec<(String, String)>,
    registries: Vec<(String, String)>,
    /// `env.sh`'s conflict or ambiguity, which the two inherit.
    blocked: Option<(State, String)>,
}

struct PlanCtx<'a> {
    registries: &'a [RegistryInfo],
    opts: &'a Options<'a>,
    env: &'a Env,
    marked: BTreeSet<(String, String)>,
    /// The managed rc file's own lines, for the `export VAR=` override scan.
    rc_user: String,
    env_out: Option<EnvOut>,
}

/// A target file as read.
struct ReadTarget {
    current: Option<String>,
    /// The file is not UTF-8 and was decoded byte per char ([`read_text`]).
    latin1: bool,
    managed: Option<Marker>,
    /// The managed part was edited since it was written.
    edited: bool,
    /// The part the user owns.
    user: String,
}

/// A file's text, `None` when it does not exist. A file that is not UTF-8
/// (a latin-1 `~/.bashrc`) is decoded one byte per char, flagged, and written
/// back the same way ([`encode`]), so its bytes survive a splice untouched.
fn read_text(path: &Path) -> Result<Option<(String, bool)>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(match String::from_utf8(bytes) {
            Ok(s) => (s, false),
            Err(e) => (
                e.into_bytes().iter().map(|&b| char::from(b)).collect(),
                true,
            ),
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// `content` as the bytes to write: UTF-8, or one byte per char for a file
/// [`read_text`] decoded that way — refused when the block holds a char that
/// has no single byte (a non-ASCII home directory in the rc line).
fn encode(path: &Path, content: &str, latin1: bool) -> Result<Vec<u8>> {
    if !latin1 {
        return Ok(content.as_bytes().to_vec());
    }
    content
        .chars()
        .map(|c| u8::try_from(u32::from(c)).ok())
        .collect::<Option<Vec<u8>>>()
        .ok_or_else(|| {
            anyhow!(
                "{} is not UTF-8, and the block holds a character that cannot be written into it",
                path.display()
            )
        })
}

fn read_target(t: &Target) -> Result<ReadTarget> {
    let (current, latin1) = match read_text(&t.path)? {
        Some((s, latin1)) => (Some(s), latin1),
        None => (None, false),
    };
    let content = current.as_deref().unwrap_or("");
    let mut read = ReadTarget {
        latin1,
        managed: None,
        edited: false,
        user: content.to_owned(),
        current: None,
    };
    match t.shape {
        Shape::Block => {
            let span = find_block(content, t.comment).with_context(|| {
                format!(
                    "{}: a batlehub setup marker line is missing or doubled; fix the file by hand",
                    t.path.display()
                )
            })?;
            if let Some(span) = span {
                read.edited = span.marker.edited(&content[span.body_start..span.body_end]);
                read.managed = Some(span.marker);
                read.user = user_part(content, t.comment)?;
            }
        }
        Shape::DropIn => {
            if let Some(m) = drop_in_marker(content, t.comment) {
                read.edited = m.edited(drop_in_body(content));
                read.managed = Some(m);
                read.user = String::new();
            }
        }
    }
    read.current = current;
    Ok(read)
}

/// The targets a client rewrites without comments, whose bare block
/// [`find_run`] recognises (and leaves alone).
fn client_rewrites(id: TargetId) -> bool {
    matches!(id, TargetId::Npmrc | TargetId::Pip | TargetId::Yarnrc)
}

/// The registry types a target needs: one for most, every type mise has
/// settings for, and one per installed env-only client.
fn target_types(id: TargetId, installed: &[ClientId]) -> Vec<&'static str> {
    match id {
        TargetId::Cargo => vec!["cargo"],
        TargetId::Npmrc | TargetId::Yarnrc => vec!["npm"],
        TargetId::Pip | TargetId::Uv => vec!["pypi"],
        TargetId::Go => vec!["goproxy"],
        TargetId::Maven => vec!["maven"],
        TargetId::Mise => MISE_TYPES.to_vec(),
        TargetId::EnvSh | TargetId::Fish | TargetId::ShellRc => installed
            .iter()
            .map(|c| match c {
                ClientId::Rustup => "rustup",
                _ => "nodedist",
            })
            .collect(),
    }
}

type Chosen<'a> = Vec<(&'static str, &'a RegistryInfo)>;

impl<'a> PlanCtx<'a> {
    fn plan_target(&mut self, id: TargetId, t: &Target) -> Result<Option<TargetPlan>> {
        let is_shell = matches!(id, TargetId::Fish | TargetId::ShellRc);
        // The selection decides whether the file is touched; what goes in it
        // is decided by every client it serves, so `--only rustup` does not
        // rewrite `env.sh` without the node variables.
        let clients = id.clients(self.env);
        if !touches(&self.opts.only, &self.opts.skip, &clients)
            || (is_shell && self.opts.shell_rc == ShellRc::None)
        {
            return Ok(None);
        }
        let installed: Vec<ClientId> = clients
            .iter()
            .copied()
            .filter(|c| self.env.installed.contains(c))
            .collect();
        let mut p = TargetPlan {
            target: id,
            path: t.path.clone(),
            clients: if installed.is_empty() {
                clients
            } else {
                installed.clone()
            },
            state: State::NotInstalled,
            registries: Vec::new(),
            detail: None,
            snippet: None,
            current: None,
            next: None,
            env: Vec::new(),
            latin1: false,
        };
        let read = match read_target(t) {
            Ok(r) => r,
            Err(e) => {
                p.conflict(format!("{e:#}"));
                self.record_env(&p, Vec::new());
                return Ok(Some(p));
            }
        };
        // The fish drop-in is written under fish, and kept up to date
        // wherever it already is — `$SHELL` is often unset in a hook.
        if id == TargetId::Fish
            && self.env.shell.as_deref() != Some("fish")
            && read.managed.is_none()
        {
            return Ok(None);
        }
        p.current = read.current.clone();
        if installed.is_empty() {
            return Ok(Some(p));
        }

        if is_shell {
            return Ok(Some(self.plan_shell(p, t, &read)));
        }

        let (chosen, ambiguous, notes, gone) =
            self.resolve_types(&target_types(id, &installed), &read)?;
        let mut marker = Marker {
            registries: chosen
                .iter()
                .map(|(ty, r)| ((*ty).to_owned(), r.name.clone()))
                .collect(),
            ..Marker::default()
        };
        p.registries = marker.names();
        if !ambiguous.is_empty() {
            p.state = State::Ambiguous;
            p.detail = Some(format!(
                "several candidates — pick with --pick <type>=<name> ({})",
                ambiguous.join("; ")
            ));
            self.record_env(&p, marker.registries);
            return Ok(Some(p));
        }
        if !notes.is_empty() {
            p.detail = Some(notes.join("; "));
        }
        // A registry the block names went away: fail closed, never rewrite the
        // file without it (the client would go back to the public upstream).
        if gone {
            p.state = State::Orphaned;
            self.record_env(&p, marker.registries);
            return Ok(Some(p));
        }

        let body = match self.body(id, &chosen, &read, &mut marker, &mut p) {
            Ok(b) => b,
            Err(e) => {
                p.conflict(format!("{e:#}"));
                self.record_env(&p, marker.registries);
                return Ok(Some(p));
            }
        };
        let p = self.finish(p, t, &read, marker.clone(), body);
        self.record_env(&p, marker.registries);
        Ok(Some(p))
    }

    /// Keep what `env.sh` resolved for the two shell targets after it.
    fn record_env(&mut self, p: &TargetPlan, registries: Vec<(String, String)>) {
        if p.target != TargetId::EnvSh {
            return;
        }
        let blocked = matches!(
            p.state,
            State::Conflict | State::Ambiguous | State::Orphaned
        )
        .then(|| {
            (
                p.state,
                format!(
                    "{} was not written: {}",
                    p.path.display(),
                    p.detail.as_deref().unwrap_or("")
                ),
            )
        });
        self.env_out = Some(EnvOut {
            vars: p.env.clone(),
            registries,
            blocked,
        });
    }

    /// The fish drop-in and the rc block: what `env.sh` resolved, or what
    /// stopped it.
    fn plan_shell(&self, mut p: TargetPlan, t: &Target, read: &ReadTarget) -> TargetPlan {
        let Some(env_out) = &self.env_out else {
            return p;
        };
        if let Some((state, why)) = &env_out.blocked {
            p.state = *state;
            p.detail = Some(why.clone());
            return p;
        }
        let marker = Marker {
            registries: env_out.registries.clone(),
            ..Marker::default()
        };
        p.registries = marker.names();
        p.env.clone_from(&env_out.vars);
        let body = (!env_out.vars.is_empty()).then(|| match p.target {
            TargetId::Fish => render_env(&env_out.vars, EnvShell::Fish),
            _ => {
                let path = rc_source_path(self.env);
                format!("[ -r {path} ] && . {path}")
            }
        });
        self.finish(p, t, read, marker, body)
    }

    fn resolve_types(
        &self,
        types: &[&'static str],
        read: &ReadTarget,
    ) -> Result<(Chosen<'a>, Vec<String>, Vec<String>, bool)> {
        let mut chosen = Vec::new();
        let mut ambiguous = Vec::new();
        let mut notes = Vec::new();
        let mut gone = false;
        for ty in types {
            // Every generic mirror at once: each fronts its own upstreams, so
            // two are not candidates for one place, and none is ambiguous.
            if *ty == "generic" {
                let mut all: Vec<&'a RegistryInfo> = self
                    .registries
                    .iter()
                    .filter(|r| r.registry_type == "generic" && is_candidate(r))
                    .collect();
                // `--pick generic=b` puts b's rules first: mise stops at the
                // first match, so b wins every upstream two mirrors share.
                if let Some(pick) = self.opts.picks.get("generic") {
                    all.sort_by_key(|r| &r.name != pick);
                }
                for (t, n) in read.managed.iter().flat_map(|m| &m.registries) {
                    if t == "generic" && !all.iter().any(|r| &r.name == n) {
                        gone = true;
                        notes.push(format!(
                            "generic registry {n} is no longer listed; `setup remove` to stop using the instance"
                        ));
                    }
                }
                chosen.extend(all.into_iter().map(|r| ("generic", r)));
                continue;
            }
            let own = read.managed.as_ref().and_then(|m| m.name_for(ty));
            let (resolution, note) =
                resolve(ty, self.registries, &self.opts.picks, own, &self.marked)?;
            notes.extend(note);
            match resolution {
                Resolution::One(r) => chosen.push((*ty, r)),
                Resolution::None => gone |= own.is_some(),
                Resolution::Ambiguous(names) => {
                    ambiguous.push(format!("{ty}: {}", names.join(", ")));
                }
            }
        }
        Ok((chosen, ambiguous, notes, gone))
    }

    fn base(&self, r: &RegistryInfo) -> Result<String> {
        base_url(r, self.opts)
    }

    /// The managed block's body, or `None` when there is nothing to write. An
    /// error is this target's conflict.
    fn body(
        &mut self,
        id: TargetId,
        chosen: &Chosen<'_>,
        read: &ReadTarget,
        marker: &mut Marker,
        p: &mut TargetPlan,
    ) -> Result<Option<String>> {
        match id {
            TargetId::EnvSh => {
                let mut vars = Vec::new();
                for (ty, r) in chosen {
                    vars.extend(env_vars_for(ty, &self.base(r)?));
                }
                p.env = vars;
                Ok((!p.env.is_empty()).then(|| render_env(&p.env, EnvShell::Sh)))
            }
            TargetId::Mise if chosen.is_empty() => Ok(None),
            TargetId::Mise => {
                let mut rules = String::new();
                let mut node = None;
                let mut skipped = Vec::new();
                // Generic pattern → the mirror it went to: a second mirror of
                // the same upstream would repeat the TOML key.
                let mut mirrored: BTreeMap<String, &str> = BTreeMap::new();
                for (ty, r) in chosen {
                    let base = self.base(r)?;
                    if *ty == "nodedist" {
                        // mise `Url::join`s onto it: without the slash the last
                        // segment is dropped.
                        node = Some(format!("{base}/nodedist/"));
                    }
                    if *ty == "generic" {
                        if r.upstreams.is_empty() {
                            skipped.push(format!(
                                "generic registry {} (the server does not say what it mirrors: \
                                 that needs `upstreams:read` granted to `*` on it)",
                                r.name
                            ));
                        }
                        // A 1:1 path mirror: each upstream maps onto it.
                        for upstream in &r.upstreams {
                            match check_upstream(upstream) {
                                Ok(u) => {
                                    let pattern = format!("^{}/(.+)", regex_escape(u));
                                    if let Some(first) = mirrored.get(&pattern) {
                                        skipped.push(format!(
                                            "generic registry {} on {u} (registry {first} has it; \
                                             --pick generic={} to swap)",
                                            r.name, r.name
                                        ));
                                        continue;
                                    }
                                    push_replacement(
                                        &mut rules,
                                        &pattern,
                                        &format!("{base}/generic/$1"),
                                    );
                                    mirrored.insert(pattern, &r.name);
                                }
                                Err(why) => {
                                    skipped.push(format!("generic registry {} ({why})", r.name))
                                }
                            }
                        }
                        continue;
                    }
                    if let Some(why) = foreign_upstream(ty, r) {
                        skipped.push(why);
                        continue;
                    }
                    for (pattern, replacement) in typed_url_replacements(ty) {
                        push_replacement(
                            &mut rules,
                            pattern,
                            &replacement.replace("{proxy}", &base),
                        );
                    }
                }
                let mut body = String::new();
                if !rules.is_empty() {
                    body.push_str("[settings.url_replacements]\n");
                    body.push_str(&rules);
                }
                // mise reads its own node mirror setting, not
                // `NODEJS_ORG_MIRROR`.
                if let Some(url) = node {
                    if !body.is_empty() {
                        body.push('\n');
                    }
                    body.push_str(&format!("[settings.node]\nmirror_url = \"{url}\"\n"));
                }
                if !skipped.is_empty() {
                    let note = format!("no mise rules for {}", skipped.join("; "));
                    p.detail = Some(match p.detail.take() {
                        Some(d) => format!("{d}; {note}"),
                        None => note,
                    });
                }
                Ok((!body.is_empty()).then_some(body))
            }
            _ => {
                let Some(&(_, r)) = chosen.first() else {
                    return Ok(None);
                };
                let base = self.base(r)?;
                if id != TargetId::Maven {
                    return Ok(Some(render_body(id, &base)));
                }
                // An XML comment may not contain `--`, and the name goes in
                // the marker comment.
                if r.name.contains("--") {
                    bail!(
                        "registry name {:?} contains `--`, which an XML comment cannot hold",
                        r.name
                    );
                }
                let mirror_of = self
                    .opts
                    .maven_mirror_of
                    .map(|m| m.mirror_of().to_owned())
                    // Read back from a marker anyone can edit, and written into
                    // `<mirrorOf>`: kept only when it is a value `apply` writes.
                    .or_else(|| {
                        read.managed
                            .as_ref()
                            .and_then(|m| m.mirror_of.clone())
                            .filter(|m| {
                                [
                                    MavenMirror::Central,
                                    MavenMirror::External,
                                    MavenMirror::All,
                                ]
                                .iter()
                                .any(|v| v.mirror_of() == m)
                            })
                    })
                    .unwrap_or_else(|| "central".to_owned());
                let body = render_maven(&r.name, &base, &mirror_of);
                marker.mirror_of = Some(mirror_of);
                Ok(Some(body))
            }
        }
    }

    /// The state, and the file's next content, once the body is known. A
    /// failure — the user's part of a TOML file does not parse — is this
    /// target's conflict.
    fn finish(
        &self,
        p: TargetPlan,
        t: &Target,
        read: &ReadTarget,
        marker: Marker,
        body: Option<String>,
    ) -> TargetPlan {
        let fallback = p.clone();
        self.try_finish(p, t, read, marker, body)
            .unwrap_or_else(|e| {
                let mut p = fallback;
                p.conflict(format!("{e:#}"));
                p
            })
    }

    fn try_finish(
        &self,
        mut p: TargetPlan,
        t: &Target,
        read: &ReadTarget,
        mut marker: Marker,
        body: Option<String>,
    ) -> Result<TargetPlan> {
        p.latin1 = read.latin1;
        if read.edited {
            p.conflict(format!(
                "{} was edited since `setup apply` wrote it, so it is left alone; {}",
                t.path.display(),
                match t.shape {
                    Shape::Block =>
                        "delete its two marker lines to keep it as yours, or the whole block to have it rewritten",
                    Shape::DropIn =>
                        "delete its first line to keep it as yours, or the file to have it rewritten",
                }
            ));
            p.snippet = body;
            return Ok(p);
        }
        let content = read.current.as_deref().unwrap_or("");
        let Some(body) = body else {
            // A block with nothing left to resolve is kept: `remove` takes it out.
            p.state = State::NoRegistry;
            if read.managed.is_some() {
                p.state = State::Orphaned;
                p.detail.get_or_insert_with(|| {
                    format!(
                        "{} names no registry the server still lists; kept — `setup remove` to take it out",
                        t.path.display()
                    )
                });
            }
            return Ok(p);
        };

        // The exact lines apply would write, already there without markers:
        // pasted from the Setup Guide, or a block a client rewrote without its
        // comments. Nothing tells the two apart, so they are never taken over
        // — the file already does what apply would, and stays the user's.
        let bare = (read.managed.is_none() && client_rewrites(p.target))
            .then(|| find_run(content, &body))
            .flatten()
            .map(|span| without(content, span));
        let user = bare.as_deref().unwrap_or(&read.user);

        if let Some(why) = self
            .conflict(p.target, t, read, user, &p.env, &body)?
            .or_else(|| yaml_key_clash(&t.path, user, &body))
        {
            p.conflict(why);
            p.snippet = Some(body);
            return Ok(p);
        }
        if bare.is_some() {
            p.state = State::UpToDate;
            p.detail = Some(format!(
                "{} already has these lines outside batlehub markers; left as yours \
                 (`remove` will not touch them)",
                t.path.display()
            ));
            return Ok(p);
        }

        let next = match t.shape {
            Shape::DropIn => {
                marker.created = true;
                render_drop_in(&marker, t.comment, &body)
            }
            Shape::Block => splice(
                read.current.as_deref(),
                t.comment,
                marker,
                &body,
                p.target == TargetId::ShellRc,
            )?,
        };
        if p.target.is_toml() {
            if let Err(e) = toml::from_str::<toml::Table>(&next) {
                p.conflict(format!(
                    "{} would not parse as TOML with the block added (an inline table \
                     defining the same keys?): {}",
                    t.path.display(),
                    e.message()
                ));
                p.snippet = Some(body);
                return Ok(p);
            }
        }
        encode(&t.path, &next, read.latin1)?;
        p.state = match (&read.current, read.managed.is_some()) {
            (Some(c), true) if *c == next => State::UpToDate,
            (_, true) => State::Drifted,
            (_, false) => State::Absent,
        };
        p.next = Some(next);
        Ok(p)
    }
}

/// A top-level YAML key the block would add and the user's part already has
/// (yarn's `unsafeHttpWhitelist`, written only for a plain-HTTP registry):
/// YAML refuses a key twice, so appending would break the whole file.
fn yaml_key_clash(path: &Path, user: &str, body: &str) -> Option<String> {
    if path.extension().is_none_or(|e| e != "yml") {
        return None;
    }
    body.lines()
        .filter_map(|l| l.strip_suffix(':').filter(|k| !k.starts_with(' ')))
        .find(|key| user.lines().any(|l| l.starts_with(&format!("{key}:"))))
        .map(|key| format!("{} already sets {key}", path.display()))
}

/// The rc file's own lines, for the `export VAR=` override scan. An rc file
/// that cannot be read or whose block is broken has no lines to scan; the rc
/// target reports the breakage itself.
fn rc_user_part(env: &Env) -> String {
    rc_path(env)
        .and_then(|path| read_text(&path).ok().flatten())
        .and_then(|(c, _)| user_part(&c, Comment::Hash).ok())
        .unwrap_or_default()
}

/// The `export K='v'` lines of an `env.sh` this module wrote, unquoted.
pub fn exports_of(content: &str) -> Vec<(String, String)> {
    content
        .lines()
        .filter_map(|l| {
            let (k, v) = l.strip_prefix("export ")?.split_once('=')?;
            let v = v.strip_prefix('\'')?.strip_suffix('\'')?;
            Some((k.to_owned(), v.replace(r"'\''", "'")))
        })
        .collect()
}

/// The variables the `env.sh` on this machine exports, when it is ours —
/// what `setup env` prints when the server cannot be reached.
///
/// The output is `eval`ed: a file edited since it was written is not trusted,
/// and only the variable names `apply` writes are printed.
pub fn cached_env(env: &Env) -> Option<Vec<(String, String)>> {
    let content = std::fs::read_to_string(env_sh_path(env)).ok()?;
    let marker = drop_in_marker(&content, Comment::Hash)?;
    if marker.sum.is_none() || marker.edited(drop_in_body(&content)) {
        return None;
    }
    Some(
        exports_of(&content)
            .into_iter()
            // The sum is no defence (anyone who can edit the file can
            // recompute it): what is printed is checked as `apply` writes it,
            // a known name and a plain http(s) URL.
            .filter(|(k, v)| env_var_client(k).is_some() && is_plain_http_url(v))
            .collect(),
    )
}

/// What `setup env` may do with the `env.sh` on disk without the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cached {
    /// Print these.
    Usable(Vec<(String, String)>),
    /// The process already holds another value for one of the names — an
    /// override the planned path refuses (exit 3) too: print nothing.
    Overridden(String),
    /// No trusted file, or one for another server.
    Unusable,
}

/// [`cached_env`], checked: with a `server`, every URL is one of its
/// registries ([`is_instance_url`]); and no variable already holds another
/// non-empty value.
pub fn cached_env_for(env: &Env, server: Option<&str>) -> Cached {
    let Some(vars) = cached_env(env).filter(|v| !v.is_empty()) else {
        return Cached::Unusable;
    };
    if server.is_some_and(|s| vars.iter().any(|(_, v)| !is_instance_url(v, s))) {
        return Cached::Unusable;
    }
    let overridden = vars.iter().find_map(|(k, v)| {
        env.vars
            .get(k)
            .filter(|cur| !cur.is_empty() && *cur != v)
            .map(|cur| format!("${k} is already set to {cur}"))
    });
    match overridden {
        Some(why) => Cached::Overridden(why),
        None => Cached::Usable(vars),
    }
}

/// The env-only client a variable `apply` writes belongs to.
pub fn env_var_client(name: &str) -> Option<ClientId> {
    if RUSTUP_VARS.contains(&name) {
        Some(ClientId::Rustup)
    } else if NODEDIST_VARS.contains(&name) {
        Some(ClientId::Node)
    } else {
        None
    }
}

/// npm reads its configuration from the environment case-insensitively.
fn npm_var<'v>(env: &'v Env, key: &str) -> Option<(&'v str, &'v str)> {
    env.vars
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(k, v)| (k.as_str(), v.as_str()))
}

/// `key` exported from the rc file's own lines, in any case (npm's rule): the
/// name as written there.
fn rc_exports_any_case(rc_user: &str, key: &str, body: &str, target: &Path) -> Option<String> {
    rc_user.lines().find_map(|l| {
        let (k, v) = l.trim().strip_prefix("export ")?.split_once('=')?;
        (k.eq_ignore_ascii_case(key) && !agrees(v.trim_matches(['"', '\'']), body, target))
            .then(|| k.to_owned())
    })
}

/// A variable that changes nothing the block does: empty, the very URL the
/// block writes (`export GOPROXY=` pasted from `setup detect`), or a
/// config-file variable naming the file the block is written to.
fn agrees(value: &str, body: &str, target: &Path) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return true;
    }
    let same_url = value.contains("://")
        && body
            .split(|c: char| c.is_whitespace() || matches!(c, '"' | '=' | ','))
            .any(|tok| tok.trim_end_matches('/') == value.trim_end_matches('/'));
    same_url || same_file(Path::new(value), target)
}

/// Both paths name one file (through a symlink, say). A missing path names
/// nothing.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// pnpm's global config files (`pnpm config set` writes `rc`, pnpm 11+
/// `auth.ini`), read before `~/.npmrc`.
fn pnpm_global_rcs(env: &Env) -> Vec<PathBuf> {
    let dirs = [
        env.config_dir.join("pnpm"),
        env.home.join("Library/Preferences/pnpm"),
    ];
    dirs.iter()
        .flat_map(|d| [d.join("rc"), d.join("auth.ini")])
        .collect()
}

/// The `[settings]` table of every other mise config file loaded with the
/// drop-in at `own`: the global config, `config.local.toml` and the other
/// `conf.d` files. mise keeps a table like `url_replacements` *whole* from the
/// last file that sets it, so any of them sharing a key with the drop-in
/// silently disables one side.
fn mise_other_settings(env: &Env, own: &Path) -> Vec<(PathBuf, toml::Table)> {
    let dir = mise_dir(env);
    let mut paths = vec![
        env.vars
            .get("MISE_GLOBAL_CONFIG_FILE")
            .filter(|p| !p.is_empty())
            .map_or_else(|| dir.join("config.toml"), PathBuf::from),
        dir.join("config.local.toml"),
    ];
    if let Ok(entries) = std::fs::read_dir(dir.join("conf.d")) {
        let mut more: Vec<PathBuf> = entries
            .filter_map(|e| Some(e.ok()?.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "toml") && p != own)
            .collect();
        more.sort();
        paths.extend(more);
    }
    paths
        .into_iter()
        .filter_map(|p| {
            let doc: toml::Table = toml::from_str(&std::fs::read_to_string(&p).ok()?).ok()?;
            let settings = doc.get("settings")?.as_table()?.clone();
            Some((p, settings))
        })
        .collect()
}

impl PlanCtx<'_> {
    /// A setting outside the block that the block would fight: in the file
    /// itself (RFC 0038 §4.3, first table), or one that overrides the file
    /// (second table). `user` is the part of the file the user owns.
    fn conflict(
        &self,
        id: TargetId,
        t: &Target,
        read: &ReadTarget,
        user: &str,
        env_out: &[(String, String)],
        body: &str,
    ) -> Result<Option<String>> {
        let env = self.env;
        let rc_user = self.rc_user.as_str();
        let path = t.path.display();
        let unmanaged_file = read.current.is_some() && read.managed.is_none();
        let lines = || user.lines().map(str::trim);
        let in_file = match id {
            TargetId::Cargo => {
                let doc: toml::Table = toml::from_str(user).with_context(|| {
                    format!("{path} is not valid TOML; fix it before running apply")
                })?;
                let has = |table: &str, key: &str| {
                    doc.get(table)
                        .and_then(|v| v.as_table())
                        .is_some_and(|t| t.contains_key(key))
                };
                if has("source", "crates-io")
                    || has("source", "batlehub")
                    || has("registries", "batlehub")
                {
                    Some(format!("{path} already sets [source.crates-io], [source.batlehub] or [registries.batlehub]"))
                } else {
                    None
                }
            }
            TargetId::Npmrc => lines()
                .any(|l| {
                    l.split_once('=')
                        .is_some_and(|(k, _)| k.trim() == "registry")
                })
                .then(|| format!("{path} already has a registry= line")),
            TargetId::Yarnrc => user
                .lines()
                .any(|l| l.starts_with("npmRegistryServer:"))
                .then(|| format!("{path} already sets npmRegistryServer")),
            // configparser reads `[global] # mine` as the section too.
            TargetId::Pip => lines()
                .any(|l| {
                    l.strip_prefix("[global]").is_some_and(|r| {
                        r.trim().is_empty() || r.trim_start().starts_with(['#', ';'])
                    })
                })
                .then(|| match body.lines().nth(1) {
                    Some(ours) if lines().any(|l| l == ours) => format!(
                        "{path} holds batlehub's index-url in a [global] section with your own \
                         keys (pip config set rewrote it without the markers): it works, but is \
                         yours now — delete the index-url line to stop using the instance"
                    ),
                    _ => format!(
                        "{path} has a [global] section: add the index-url line below to it by hand"
                    ),
                })
                // An index in another section (`[install]` wins over
                // `[global]`), or an extra one pip also asks: a version the
                // instance withholds would install from there.
                .or_else(|| {
                    lines()
                        .filter_map(|l| l.split_once('=').map(|(k, _)| k.trim()))
                        .find(|k| matches!(*k, "index-url" | "extra-index-url" | "find-links"))
                        .map(|k| format!("{path} sets {k} outside the block, which pip reads with or over it"))
                }),
            TargetId::Uv => {
                let doc: toml::Table = toml::from_str(user).with_context(|| {
                    format!("{path} is not valid TOML; fix it before running apply")
                })?;
                let default_index = doc
                    .get("index")
                    .and_then(|v| v.as_array())
                    .is_some_and(|a| {
                        a.iter()
                            .any(|i| i.get("default").and_then(toml::Value::as_bool) == Some(true))
                    });
                // Any other `[[index]]` is asked before the default one.
                let other_index = doc
                    .get("index")
                    .and_then(|v| v.as_array())
                    .is_some_and(|a| !a.is_empty());
                if default_index || doc.contains_key("index-url") {
                    Some(format!("{path} already sets a default index"))
                } else if other_index
                    || ["extra-index-url", "find-links"]
                        .iter()
                        .any(|k| doc.contains_key(*k))
                {
                    Some(format!(
                        "{path} declares another index, which uv asks before the default one"
                    ))
                } else {
                    None
                }
            }
            TargetId::Go => lines()
                .any(|l| l.starts_with("GOPROXY="))
                .then(|| format!("{path} already sets GOPROXY")),
            TargetId::Maven | TargetId::Mise | TargetId::EnvSh | TargetId::Fish => unmanaged_file
                .then(|| format!("{path} exists and was not written by batlehub setup")),
            TargetId::ShellRc => None,
        };
        if in_file.is_some() {
            return Ok(in_file);
        }

        // What our own env.sh exports now: a shell that sourced it carries those
        // values, and they are not the user's.
        let ours: BTreeMap<String, String> = match (id, &read.managed, &read.current) {
            (TargetId::EnvSh, Some(_), Some(c)) => exports_of(c).into_iter().collect(),
            _ => BTreeMap::new(),
        };
        // Overrides: a variable in the environment, or exported from the rc file
        // `--shell-rc` manages. One exported from any other dotfile is not seen.
        let exported = |name: &str| -> Option<String> {
            if let Some(v) = env.vars.get(name) {
                return Some(v.clone());
            }
            rc_user.lines().find_map(|l| {
                l.trim()
                    .strip_prefix("export ")?
                    .strip_prefix(name)?
                    .strip_prefix('=')
                    .map(|v| v.trim_matches(['"', '\'']).to_owned())
            })
        };
        let var = |names: &[&str]| -> Option<String> {
            names
                .iter()
                .find(|n| exported(n).is_some_and(|v| !agrees(&v, body, &t.path)))
                .map(|n| format!("${n} is set and overrides {path}"))
        };
        Ok(match id {
            TargetId::Cargo => {
                let legacy = env.cargo_home.join("config");
                // cargo recommends symlinking `config` to `config.toml` for old
                // toolchains: then both are this file.
                if legacy.is_file() && !same_file(&legacy, &t.path) {
                    Some(format!(
                        "{} exists and cargo reads it instead of {path}",
                        legacy.display()
                    ))
                } else {
                    // Only what replaces the block's own keys: a token for
                    // another registry (`CARGO_REGISTRIES_MYCO_TOKEN`) does not.
                    var(&[
                        "CARGO_SOURCE_CRATES_IO_REPLACE_WITH",
                        "CARGO_SOURCE_BATLEHUB_REGISTRY",
                        "CARGO_REGISTRIES_BATLEHUB_INDEX",
                    ])
                }
            }
            // npm reads `npm_config_*` in any case; pnpm also its own
            // `pnpm_config_registry` and its global rc, which win over ~/.npmrc.
            TargetId::Npmrc => [
                "npm_config_registry",
                "npm_config_userconfig",
                "pnpm_config_registry",
            ]
            .iter()
            .find_map(|k| {
                npm_var(env, k)
                    .filter(|(_, v)| !agrees(v, body, &t.path))
                    .map(|(k, _)| k.to_owned())
                    .or_else(|| rc_exports_any_case(rc_user, k, body, &t.path))
            })
            .map(|k| format!("${k} is set and overrides {path}"))
            .or_else(|| {
                pnpm_global_rcs(env)
                    .into_iter()
                    .find(|f| {
                        std::fs::read_to_string(f).is_ok_and(|c| {
                            c.lines().any(|l| {
                                l.split_once('=')
                                    .is_some_and(|(k, _)| k.trim() == "registry")
                            })
                        })
                    })
                    .map(|f| {
                        format!(
                            "{} sets registry= and pnpm reads it over {path}",
                            f.display()
                        )
                    })
            }),
            TargetId::Yarnrc => var(&["YARN_NPM_REGISTRY_SERVER", "YARN_RC_FILENAME"]),
            // PIP_CONFIG_FILE is read on top of the user file, so it overrides
            // only an index-url of its own — or every file, as `os.devnull`.
            // pip skips the user file entirely when PIP_CONFIG_FILE names one
            // that exists (`os.devnull` included).
            TargetId::Pip => var(&["PIP_INDEX_URL", "PIP_EXTRA_INDEX_URL", "PIP_FIND_LINKS"])
                .or_else(|| {
                    let file = env.vars.get("PIP_CONFIG_FILE").filter(|f| !f.is_empty())?;
                    (Path::new(file).exists() && !same_file(Path::new(file), &t.path))
                        .then(|| format!("$PIP_CONFIG_FILE ({file}) replaces {path}"))
                }),
            TargetId::Uv => var(&[
                "UV_EXTRA_INDEX_URL",
                "UV_FIND_LINKS",
                "UV_INDEX_URL",
                "UV_DEFAULT_INDEX",
                "UV_INDEX",
                "UV_CONFIG_FILE",
                "UV_NO_CONFIG",
            ]),
            TargetId::Go => var(&["GOPROXY"]),
            TargetId::Mise => {
                let others = mise_other_settings(env, &t.path);
                let set_in = |key: &str| {
                    others.iter().find_map(|(p, s)| {
                        let has = match key.split_once('.') {
                            Some((table, k)) => s
                                .get(table)
                                .and_then(toml::Value::as_table)
                                .is_some_and(|t| t.contains_key(k)),
                            None => s.contains_key(key),
                        };
                        has.then(|| {
                            format!(
                                "{} also sets {key}: mise keeps only one file's, so either it or {path} is ignored",
                                p.display()
                            )
                        })
                    })
                };
                if let Some(v) = var(&["MISE_URL_REPLACEMENTS"]) {
                    Some(v)
                } else if let Some(why) = body
                    .contains("[settings.url_replacements]")
                    .then(|| set_in("url_replacements"))
                    .flatten()
                {
                    Some(why)
                } else if body.contains("[settings.node]") {
                    var(&["MISE_NODE_MIRROR_URL"]).or_else(|| set_in("node.mirror_url"))
                } else {
                    None
                }
            }
            // A registry URL of this instance is its own, whoever exported it —
            // typically a shell that sourced the env.sh of a registry since
            // renamed, which only a new shell forgets. An empty value is unset.
            TargetId::EnvSh => env_out.iter().find_map(|(k, want)| {
                exported(k)
                    .filter(|have| {
                        !have.is_empty()
                            && have != want
                            && ours.get(k) != Some(have)
                            && !is_instance_url(have, self.opts.server)
                    })
                    .map(|have| format!("${k} is already set to {have}"))
            }),
            _ => None,
        })
    }
}

// ── Bodies ────────────────────────────────────────────────────────────────────

/// The block for a single-registry target. The URL shapes are the console's
/// (`ui/src/config/registryTypes.ts`); the 0005-bis drift gate compares the
/// console with the docs only, so `rendered_blocks` below is what pins these.
pub(crate) fn render_body(id: TargetId, base: &str) -> String {
    match id {
        TargetId::Cargo => format!(
            "[source.crates-io]\n\
             replace-with = \"batlehub\"\n\
             \n\
             [source.batlehub]\n\
             registry = \"sparse+{base}/registry/\"\n\
             \n\
             [registries.batlehub]\n\
             index = \"sparse+{base}/registry/\""
        ),
        TargetId::Npmrc => format!("registry={base}/"),
        // yarn 4 refuses a plain-HTTP registry it was not told to trust
        // (YN0081), and `apply` writes one only for loopback or
        // `--insecure-http`: the host goes on the allow-list with it.
        TargetId::Yarnrc if base.starts_with("http://") => format!(
            "npmRegistryServer: \"{base}/\"\nunsafeHttpWhitelist:\n  - \"{}\"",
            url_host(base).unwrap_or_default()
        ),
        TargetId::Yarnrc => format!("npmRegistryServer: \"{base}/\""),
        // pip ignores a plain-HTTP index on a host it does not trust.
        TargetId::Pip if base.starts_with("http://") => format!(
            "[global]\nindex-url = {base}/simple/\ntrusted-host = {}",
            url_host(base).unwrap_or_default()
        ),
        TargetId::Pip => format!("[global]\nindex-url = {base}/simple/"),
        TargetId::Uv => format!(
            "[[index]]\n\
             name = \"batlehub\"\n\
             url = \"{base}/simple/\"\n\
             default = true"
        ),
        // No `,direct`: a module the instance refuses must fail, not be fetched
        // around it. No GOSUMDB: go proxies the checksum database through
        // GOPROXY on its own (`$GOPROXY/sumdb/sum.golang.org/supported`).
        TargetId::Go => format!("GOPROXY={base}"),
        _ => String::new(),
    }
}

fn render_maven(name: &str, base: &str, mirror_of: &str) -> String {
    format!(
        "<settings>\n\
         \x20 <mirrors>\n\
         \x20   <mirror>\n\
         \x20     <id>batlehub-{name}</id>\n\
         \x20     <name>BatleHub Maven Proxy</name>\n\
         \x20     <url>{base}/maven2/</url>\n\
         \x20     <mirrorOf>{mirror_of}</mirrorOf>\n\
         \x20   </mirror>\n\
         \x20 </mirrors>\n\
         </settings>"
    )
}

const RUSTUP_VARS: [&str; 2] = ["RUSTUP_DIST_SERVER", "RUSTUP_UPDATE_ROOT"];

/// The variables nvm, fnm, n and node-gyp read for a node mirror, in that
/// order — the one list `setup env` and `registry suggest --client-env` share.
pub(crate) const NODEDIST_VARS: [&str; 4] = [
    "NVM_NODEJS_ORG_MIRROR",
    "FNM_NODE_DIST_MIRROR",
    "N_NODE_MIRROR",
    "NODEJS_ORG_MIRROR",
];

/// The variables an env-only registry needs, as the console prints them.
fn env_vars_for(registry_type: &str, base: &str) -> Vec<(String, String)> {
    match registry_type {
        "rustup" => vec![
            ("RUSTUP_DIST_SERVER".to_owned(), format!("{base}/rustup")),
            (
                "RUSTUP_UPDATE_ROOT".to_owned(),
                format!("{base}/rustup/rustup"),
            ),
        ],
        "nodedist" => NODEDIST_VARS
            .iter()
            .map(|k| ((*k).to_owned(), format!("{base}/nodedist")))
            .collect(),
        _ => vec![],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum EnvShell {
    Sh,
    Fish,
}

pub fn render_env(vars: &[(String, String)], shell: EnvShell) -> String {
    vars.iter()
        .map(|(k, v)| match shell {
            EnvShell::Sh => format!("export {k}={}\n", shell_quote(v)),
            EnvShell::Fish => format!("set -gx {k} {}\n", shell_quote(v)),
        })
        .collect()
}

/// `env.sh`'s path as the rc line spells it: from `"$HOME"` when it is under
/// the home directory, so a home mounted elsewhere still finds it; quoted
/// absolute otherwise.
fn rc_source_path(env: &Env) -> String {
    let path = env_sh_path(env);
    match path.strip_prefix(&env.home).ok().and_then(Path::to_str) {
        Some(rel) if !rel.contains(['"', '$', '`', '\\', '!']) => format!("\"$HOME/{rel}\""),
        _ => shell_quote(&path.to_string_lossy()),
    }
}

/// Single-quote for sh and fish alike; a URL never carries a `'` (base_url
/// refuses one), but a home directory might.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// ── Writing ───────────────────────────────────────────────────────────────────

/// Write `content` to `path` atomically — a sibling temp file of its own,
/// fsync, rename — keeping the original's mode. The temp file is created with
/// that mode, so a `0600` `~/.npmrc` holding a token is never copied into a
/// world-readable file, and with a name no concurrent run shares. A file the
/// user made read-only is refused, not replaced; a file that cannot be
/// replaced (a bind mount, Che's `subPath` automount) is written in place.
/// A hard link is broken by the rename.
///
/// A symlink (a dotfile manager's) is written through to its target, never
/// replaced by a regular file — unless the target belongs to someone other
/// than the link's owner: as root with a user's `HOME`, a `~/.bashrc` linked
/// to `/root/.bashrc` would otherwise have root append a line sourcing a file
/// the user can write. A dangling link creates its target.
pub fn write_atomic(path: &Path, content: &[u8]) -> Result<()> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);

    let path = resolve_link(path)?;
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("{} has no file name", path.display()))?
        .to_string_lossy();
    let tmp = dir.join(format!(
        ".{name}.batlehub-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let original = std::fs::metadata(&path).ok();
    #[cfg(unix)]
    if let Some(m) = &original {
        use std::os::unix::fs::PermissionsExt;
        if m.permissions().mode() & 0o222 == 0 {
            bail!("{} is read-only; left alone", path.display());
        }
    }
    let result = (|| -> Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            opts.mode(
                original
                    .as_ref()
                    .map_or(0o666, |m| m.permissions().mode() & 0o7777),
            );
        }
        let mut f = opts.open(&tmp)?;
        if let Some(m) = &original {
            f.set_permissions(m.permissions())?;
        }
        f.write_all(content)?;
        f.sync_all()?;
        match std::fs::rename(&tmp, &path) {
            // EBUSY: a mount point cannot be renamed over.
            Err(e) if e.raw_os_error() == Some(16) => {
                std::fs::write(&path, content)?;
                // The content is in place; a leftover temp file is litter, not a failure.
                std::fs::remove_file(&tmp).ok();
            }
            r => r?,
        }
        Ok(())
    })();
    if result.is_err() {
        // Best-effort cleanup: the write error is what the caller needs to see.
        std::fs::remove_file(&tmp).ok();
    }
    result.with_context(|| format!("writing {}", path.display()))
}

/// The file a write to `path` lands in: `path` itself, or a symlink's target
/// (created when dangling), refused when owned by another user than the link.
/// Only the last component is checked: running as root with a user's `HOME`,
/// where a symlinked parent would matter, is refused up front
/// ([`check_not_root_for_another`]).
fn resolve_link(path: &Path) -> Result<PathBuf> {
    if !path.is_symlink() {
        return Ok(path.to_owned());
    }
    let target = match std::fs::canonicalize(path) {
        Ok(t) => t,
        Err(_) => {
            let link = std::fs::read_link(path)
                .with_context(|| format!("resolving {}", path.display()))?;
            path.parent().map_or(link.clone(), |p| p.join(&link))
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let link = std::fs::symlink_metadata(path)?;
        if let Ok(meta) = std::fs::metadata(&target) {
            if meta.uid() != link.uid() {
                bail!(
                    "{} links to {}, which another user owns; refusing to write through it",
                    path.display(),
                    target.display()
                );
            }
        }
    }
    Ok(target)
}

/// Delete `path`. Through a symlink, the target goes and the link stays: the
/// link is the user's (a dotfile manager's), the file is what `apply`
/// created through it.
fn delete(path: &Path) -> Result<()> {
    let path = resolve_link(path)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("deleting {}", path.display())),
    }
}

/// Write `next` to `path`, or delete it for `None`.
fn write_or_delete(path: &Path, next: Option<&str>, latin1: bool) -> Result<()> {
    match next {
        Some(content) => write_atomic(path, &encode(path, content, latin1)?),
        None => delete(path),
    }
}

/// Refuse to write as root into a home root does not own. Every path here
/// is the user's to plant — a `~/.config` symlinked to `/etc/xdg`, a
/// `~/.bashrc` to a file in `/etc/profile.d` — and following them as root
/// writes system files. A devcontainer hook runs `apply` as the user
/// (`su -c`, `runuser`), not as root.
pub fn check_not_root_for_another(env: &Env) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let euid = rustix::process::geteuid().as_raw();
        let home_owner = std::fs::metadata(&env.home)
            .with_context(|| format!("reading {}", env.home.display()))?
            .uid();
        if euid == 0 && home_owner != 0 {
            bail!(
                "running as root with HOME={} (owned by uid {home_owner}): run setup as that \
                 user (`runuser -u <user> -- batlehub-cli setup …`), not as root",
                env.home.display()
            );
        }
    }
    Ok(())
}

/// Apply one plan entry: write its next content, or delete the file.
pub fn apply_one(p: &TargetPlan) -> Result<()> {
    write_or_delete(&p.path, p.next.as_deref(), p.latin1)
}

/// A path in `--json` output: serde refuses a non-UTF-8 one, and `json!`
/// panics on the refusal — after the files were written.
pub fn lossy_path<S: serde::Serializer>(p: &Path, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(&p.to_string_lossy())
}

/// What `remove` does to one file.
#[derive(Debug, Clone, Serialize)]
pub struct Removal {
    #[serde(serialize_with = "lossy_path")]
    pub path: PathBuf,
    /// `None`: the file is deleted.
    #[serde(skip)]
    pub next: Option<String>,
    pub deletes: bool,
    #[serde(skip)]
    pub current: String,
    #[serde(skip)]
    latin1: bool,
}

/// Every managed part on this machine, what the file becomes without it, and
/// what was left alone and why. Needs no server: the markers say everything.
/// `server`, when known, only lets `remove` point out a file a client
/// stripped of its markers — which it does not edit: without the registry
/// list it cannot tell our line from one the user wrote.
///
/// Both rc files and go's env file are looked at whatever this process can
/// see of the shell or of go (`remove` runs from hooks with neither), unless
/// `GOENV=off` now hides go's. A file that cannot be read or has broken
/// markers, and a block or drop-in edited since it was written, are reported
/// and left alone; the others are still removed. `--only`/`--skip` select
/// files as `apply` does: `--only rustup` takes out `env.sh`, node's
/// variables with it.
pub fn removals(
    env: &Env,
    only: &[ClientId],
    skip: &[ClientId],
    server: Option<&str>,
) -> (Vec<Removal>, Vec<String>) {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    // Path-routed (`{server}/proxy/…`) or on a registry's own host.
    let points_at_server = |content: &str| {
        server.is_some_and(|s| {
            content
                .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '='))
                .any(|tok| is_instance_url(tok, s))
        })
    };
    for id in ALL_TARGETS {
        let clients = match id {
            TargetId::Yarnrc => vec![ClientId::Yarn],
            _ => id.clients(env),
        };
        if !touches(only, skip, &clients) {
            continue;
        }
        for t in targets(id, env, true) {
            if !seen.insert(t.path.clone()) {
                continue;
            }
            let (content, latin1) = match read_text(&t.path) {
                Ok(Some(c)) => c,
                Ok(None) => continue,
                Err(e) => {
                    warnings.push(format!("{e:#}; left alone"));
                    continue;
                }
            };
            let left_alone = |why: &str| format!("{} {why}; left alone", t.path.display());
            let next = match t.shape {
                Shape::DropIn => match drop_in_marker(&content, t.comment) {
                    // The marker pushed off the first line (an XML declaration
                    // prepended to settings.xml): edited, so said, not skipped.
                    None if content.lines().any(|l| is_start_line(t.comment, l)) => {
                        warnings.push(left_alone(
                            "has the batlehub marker below its first line, so it was edited since `setup apply` wrote it — delete it by hand if it is no longer wanted",
                        ));
                        continue;
                    }
                    None => continue,
                    Some(m) if m.edited(drop_in_body(&content)) => {
                        warnings.push(left_alone(
                            "was edited since `setup apply` wrote it — delete it by hand if it is no longer wanted",
                        ));
                        continue;
                    }
                    Some(_) => None,
                },
                Shape::Block => match find_block(&content, t.comment) {
                    Err(e) => {
                        warnings.push(left_alone(&format!("{e} — fix the markers by hand")));
                        continue;
                    }
                    Ok(Some(span))
                        if span.marker.edited(&content[span.body_start..span.body_end]) =>
                    {
                        warnings.push(left_alone(
                            "has a batlehub block edited since `setup apply` wrote it — delete it by hand if it is no longer wanted",
                        ));
                        continue;
                    }
                    Ok(Some(_)) => match unsplice(&content, t.comment) {
                        Ok(n) => n,
                        Err(e) => {
                            warnings.push(left_alone(&e.to_string()));
                            continue;
                        }
                    },
                    Ok(None) => {
                        if client_rewrites(id) && points_at_server(&content) {
                            warnings.push(left_alone(
                                "points at the server but has no batlehub markers (written by hand, or the client rewrote it) — delete those lines by hand if they are no longer wanted",
                            ));
                        }
                        continue;
                    }
                },
            };
            out.push(Removal {
                deletes: next.is_none(),
                path: t.path,
                next,
                current: content,
                latin1,
            });
        }
    }
    (out, warnings)
}

pub fn remove_one(r: &Removal) -> Result<()> {
    write_or_delete(&r.path, r.next.as_deref(), r.latin1)
}

/// A minimal diff: the lines that go and the lines that come. A splice only
/// ever replaces one contiguous span, so the common prefix and suffix are the
/// whole context there is.
pub fn diff(path: &Path, old: &str, new: Option<&str>) -> String {
    let new = new.unwrap_or("");
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let mut out = format!(
        "--- {0}\n+++ {0}\n@@ line {1} @@\n",
        path.display(),
        head + 1
    );
    for l in &a[head..a.len() - tail] {
        out.push_str(&format!("-{l}\n"));
    }
    for l in &b[head..b.len() - tail] {
        out.push_str(&format!("+{l}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn reg(name: &str, ty: &str, mode: &str) -> RegistryInfo {
        RegistryInfo {
            name: name.to_owned(),
            registry_type: ty.to_owned(),
            mode: mode.to_owned(),
            public_url: None,
            // What the server names for a registry left on its default.
            upstream: match ty {
                "github" => Some("https://api.github.com".to_owned()),
                "forgejo" => Some("https://codeberg.org".to_owned()),
                "npm" => Some("https://registry.npmjs.org".to_owned()),
                "cargo" => Some("https://crates.io".to_owned()),
                "openvsx" => Some("https://open-vsx.org".to_owned()),
                _ => None,
            },
            installable: None,
            upstreams: Vec::new(),
        }
    }

    /// A registry in front of another forge, or one whose upstream the server
    /// does not name, gets no mise rules: they would take over the public host.
    #[test]
    fn mise_rules_only_for_a_registry_of_the_public_host() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Mise]);
        let mut corp = reg("fj", "forgejo", "proxy");
        corp.upstream = Some("https://git.corp.example".to_owned());
        let mut unknown = reg("gh", "github", "proxy");
        unknown.upstream = None;
        let regs = vec![corp, unknown, reg("o", "openvsx", "proxy")];
        let p = find(&plan(&regs, &opts(), &env).unwrap(), TargetId::Mise).clone();
        let next = p.next.clone().unwrap();
        assert!(!next.contains("codeberg\\\\.org"), "{next}");
        assert!(!next.contains("github\\\\.com"), "{next}");
        assert!(next.contains("open-vsx"), "{next}");
        let detail = p.detail.unwrap();
        assert!(detail.contains("proxies git.corp.example"), "{detail}");
        assert!(detail.contains("does not say"), "{detail}");

        // Nothing left to write: no drop-in at all.
        let mut only = reg("fj", "forgejo", "proxy");
        only.upstream = Some("https://git.corp.example".to_owned());
        let p = find(&plan(&[only], &opts(), &env).unwrap(), TargetId::Mise).clone();
        assert!(!p.writes(), "{:?}", p.state);
    }

    /// Every generic mirror gets a rule per upstream, after the typed rules;
    /// one the list no longer names keeps the drop-in (fail closed).
    #[test]
    fn mise_rules_for_every_generic_mirror() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Mise]);
        let mut hc = reg("hc", "generic", "proxy");
        hc.upstreams = vec!["https://releases.hashicorp.com/".to_owned()];
        let mut k8s = reg("k8s", "generic", "proxy");
        k8s.upstreams = vec!["https://dl.k8s.io".to_owned(), "bad url".to_owned()];
        let regs = vec![hc, k8s.clone(), reg("gh", "github", "proxy")];
        let plans = plan(&regs, &opts(), &env).unwrap();
        let p = find(&plans, TargetId::Mise).clone();
        let next = p.next.clone().unwrap();
        let hc_rule = r#""regex:^https://releases\\.hashicorp\\.com/(.+)" = "https://hub.example.com/proxy/hc/generic/$1""#;
        assert!(next.contains(hc_rule), "{next}");
        assert!(next.contains("proxy/k8s/generic/$1"), "{next}");
        assert!(
            next.find("api\\\\.github").unwrap() < next.find("hashicorp").unwrap(),
            "typed rules first: {next}"
        );
        assert!(p.detail.unwrap().contains("not a plain http(s) URL"));
        toml::from_str::<toml::Table>(&next).unwrap();

        apply_all(&plans);
        let p = find(
            &plan(&[k8s, reg("gh", "github", "proxy")], &opts(), &env).unwrap(),
            TargetId::Mise,
        )
        .clone();
        assert_eq!(p.state, State::Orphaned, "{:?}", p.detail);
        assert!(p
            .detail
            .unwrap()
            .contains("generic registry hc is no longer listed"));
    }

    /// Two mirrors of one upstream: one rule (a repeated TOML key would not
    /// parse), and `--pick generic=b` decides whose.
    #[test]
    fn a_generic_pick_wins_a_shared_upstream() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Mise]);
        let regs: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|n| {
                let mut r = reg(n, "generic", "proxy");
                r.upstreams = vec!["https://dl.k8s.io".to_owned()];
                r
            })
            .collect();
        let mut o = opts();
        o.picks.insert("generic".to_owned(), "b".to_owned());
        let p = find(&plan(&regs, &o, &env).unwrap(), TargetId::Mise).clone();
        let next = p
            .next
            .clone()
            .unwrap_or_else(|| panic!("{:?} {:?}", p.state, p.detail));
        assert!(
            next.contains("proxy/b/") && !next.contains("proxy/a/"),
            "{next}"
        );
        assert!(p.detail.unwrap().contains("registry b has it"));
        toml::from_str::<toml::Table>(&next).unwrap();
    }

    const SERVER: &str = "https://hub.example.com";

    fn env_in(dir: &Path, installed: &[ClientId]) -> Env {
        Env {
            home: dir.to_owned(),
            config_dir: dir.join(".config"),
            cargo_home: dir.join(".cargo"),
            goenv: Some(dir.join(".config/go/env")),
            pip_dir: dir.join(".config/pip"),
            shell: Some("bash".to_owned()),
            vars: BTreeMap::new(),
            // `node` is always installed, as `Env::from_process` has it.
            installed: installed.iter().copied().chain([ClientId::Node]).collect(),
            yarn_major: installed.contains(&ClientId::Yarn).then_some(4),
        }
    }

    fn opts() -> Options<'static> {
        Options {
            server: SERVER,
            picks: RegistryPicks::new(),
            maven_mirror_of: None,
            shell_rc: ShellRc::Auto,
            only: vec![],
            skip: vec![],
            allow_foreign_hosts: false,
            insecure_http: false,
        }
    }

    fn find(plans: &[TargetPlan], id: TargetId) -> &TargetPlan {
        plans.iter().find(|p| p.target == id).unwrap()
    }

    fn apply_all(plans: &[TargetPlan]) {
        for p in plans.iter().filter(|p| p.writes()) {
            apply_one(p).unwrap();
        }
    }

    // ── splice / unsplice ──

    fn round_trip(original: &str) {
        let marker = Marker {
            registries: vec![("npm".into(), "npm1".into())],
            ..Marker::default()
        };
        let spliced = splice(Some(original), Comment::Hash, marker, "registry=x/", false).unwrap();
        assert!(spliced.starts_with(original), "{spliced:?}");
        assert_eq!(
            unsplice(&spliced, Comment::Hash).unwrap().as_deref(),
            Some(original),
            "{spliced:?}"
        );
    }

    #[test]
    fn splice_round_trips_byte_for_byte() {
        round_trip("");
        round_trip("a=1\n");
        round_trip("a=1"); // no final newline: the marker's `nl`
        round_trip("a=1\r\nb=2\r\n");
        round_trip("\u{feff}a=1\n");
        round_trip("a=1\n\n\n");
    }

    #[test]
    fn splice_replaces_in_place_and_keeps_user_lines_after_the_block() {
        let m = Marker::default();
        let first = splice(Some("a=1"), Comment::Hash, m.clone(), "x=1", false).unwrap();
        let edited = format!("{first}after=1\n");
        let second = splice(Some(&edited), Comment::Hash, m, "x=2", false).unwrap();
        assert!(second.contains("x=2") && !second.contains("x=1"));
        assert!(second.ends_with("after=1\n"));
        // With a line after the block, the newline `nl` recorded stays: taking
        // it out would glue `a=1` to `after=1`.
        assert_eq!(
            unsplice(&second, Comment::Hash).unwrap().unwrap(),
            "a=1\nafter=1\n"
        );
    }

    #[test]
    fn a_created_file_is_deleted_by_unsplice() {
        let s = splice(None, Comment::Hash, Marker::default(), "x=1", false).unwrap();
        assert_eq!(unsplice(&s, Comment::Hash).unwrap(), None);
        // An empty file that existed is kept, empty.
        let s = splice(Some(""), Comment::Hash, Marker::default(), "x=1", false).unwrap();
        assert_eq!(unsplice(&s, Comment::Hash).unwrap().as_deref(), Some(""));
    }

    #[test]
    fn a_half_block_refuses() {
        let start = Marker::default().render(Comment::Hash);
        assert!(find_block(&format!("{start}\nx=1\n"), Comment::Hash).is_err());
        assert!(find_block("# <<< batlehub setup <<<\n", Comment::Hash).is_err());
        let block = render_block(&Marker::default(), Comment::Hash, "x");
        assert!(find_block(&format!("{block}{block}"), Comment::Hash).is_err());
    }

    #[test]
    fn marker_round_trips() {
        let m = Marker {
            registries: vec![("maven".into(), "a".into()), (String::new(), "b".into())],
            created: true,
            nl: true,
            top: true,
            mirror_of: Some("external:*".into()),
            sum: Some(body_sum("x")),
        };
        for c in [Comment::Hash, Comment::Xml] {
            assert_eq!(Marker::parse(c, &m.render(c)), Some(m.clone()));
        }
        // An XML comment may not contain `--`.
        let xml = m.render(Comment::Xml);
        assert!(!xml
            .trim_start_matches("<!--")
            .trim_end_matches("-->")
            .contains("--"));
    }

    // ── plan ──

    #[test]
    fn plan_reports_every_state() {
        let dir = TempDir::new().unwrap();
        let env = env_in(
            dir.path(),
            &[ClientId::Npm, ClientId::Cargo, ClientId::Pip, ClientId::Go],
        );
        std::fs::create_dir_all(dir.path().join(".config/pip")).unwrap();
        std::fs::write(
            dir.path().join(".config/pip/pip.conf"),
            "[global]\ntimeout = 5\n",
        )
        .unwrap();
        let regs = vec![
            reg("npm1", "npm", "proxy"),
            reg("crates", "cargo", "hybrid"),
            reg("py", "pypi", "proxy"),
            reg("go1", "goproxy", "proxy"),
            reg("go2", "goproxy", "proxy"),
        ];
        let plans = plan(&regs, &opts(), &env).unwrap();
        assert_eq!(find(&plans, TargetId::Npmrc).state, State::Absent);
        assert_eq!(find(&plans, TargetId::Cargo).state, State::Absent);
        assert_eq!(find(&plans, TargetId::Pip).state, State::Conflict);
        assert_eq!(find(&plans, TargetId::Go).state, State::Ambiguous);
        assert_eq!(find(&plans, TargetId::Uv).state, State::NotInstalled);
        assert_eq!(find(&plans, TargetId::Maven).state, State::NotInstalled);

        apply_all(&plans);
        let again = plan(&regs, &opts(), &env).unwrap();
        assert_eq!(find(&again, TargetId::Npmrc).state, State::UpToDate);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".npmrc"))
                .unwrap()
                .lines()
                .nth(1),
            Some("registry=https://hub.example.com/proxy/npm1/")
        );

        // The instance changed: drifted.
        let renamed = vec![reg("npm2", "npm", "proxy")];
        assert_eq!(
            find(&plan(&renamed, &opts(), &env).unwrap(), TargetId::Npmrc).state,
            State::Drifted
        );
        // The registry is gone: orphaned, and the block is kept (fail
        // closed) — only `remove` takes it out.
        let other = vec![reg("p", "pypi", "proxy")];
        let p = find(&plan(&other, &opts(), &env).unwrap(), TargetId::Npmrc).clone();
        assert_eq!(p.state, State::Orphaned);
        assert!(!p.writes());
        assert!(
            p.detail.as_deref().unwrap().contains("no longer listed"),
            "{:?}",
            p.detail
        );
    }

    #[test]
    fn a_registry_the_caller_cannot_install_from_is_not_a_candidate() {
        let mut meta_only = reg("m", "npm", "proxy");
        meta_only.installable = Some(false);
        let regs = vec![meta_only, reg("ok", "npm", "proxy")];
        assert_eq!(
            resolve("npm", &regs, &RegistryPicks::new(), None, &BTreeSet::new()).unwrap(),
            (Resolution::One(&regs[1]), None)
        );
    }

    #[test]
    fn local_registries_are_not_candidates() {
        let regs = vec![reg("pub", "npm", "proxy"), reg("priv", "npm", "local")];
        assert_eq!(
            resolve("npm", &regs, &RegistryPicks::new(), None, &BTreeSet::new()).unwrap(),
            (Resolution::One(&regs[0]), None)
        );
        let only_local = vec![reg("priv", "npm", "local")];
        assert_eq!(
            resolve(
                "npm",
                &only_local,
                &RegistryPicks::new(),
                None,
                &BTreeSet::new()
            )
            .unwrap(),
            (Resolution::None, None)
        );
    }

    #[test]
    fn a_pick_is_kept_by_the_next_run_without_the_flag() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Npm, ClientId::Yarn]);
        let regs = vec![reg("a", "npm", "proxy"), reg("b", "npm", "proxy")];
        assert_eq!(
            find(&plan(&regs, &opts(), &env).unwrap(), TargetId::Npmrc).state,
            State::Ambiguous
        );

        let mut picked = opts();
        picked.picks = parse_picks(&["npm=b".to_owned()]).unwrap();
        apply_all(&plan(&regs, &picked, &env).unwrap());

        let plans = plan(&regs, &opts(), &env).unwrap();
        for id in [TargetId::Npmrc, TargetId::Yarnrc] {
            assert_eq!(find(&plans, id).state, State::UpToDate, "{id:?}");
        }
        assert!(std::fs::read_to_string(dir.path().join(".yarnrc.yml"))
            .unwrap()
            .contains("/proxy/b/"));
    }

    #[test]
    fn maven_mirror_of_is_kept_and_defaults_to_central() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Maven]);
        let regs = vec![reg("m", "maven", "proxy")];
        let settings = dir.path().join(".m2/settings.xml");

        apply_all(&plan(&regs, &opts(), &env).unwrap());
        assert!(std::fs::read_to_string(&settings)
            .unwrap()
            .contains("<mirrorOf>central</mirrorOf>"));

        let mut star = opts();
        star.maven_mirror_of = Some(MavenMirror::All);
        apply_all(&plan(&regs, &star, &env).unwrap());
        assert_eq!(
            find(&plan(&regs, &opts(), &env).unwrap(), TargetId::Maven).state,
            State::UpToDate
        );
        assert!(std::fs::read_to_string(&settings)
            .unwrap()
            .contains("<mirrorOf>*</mirrorOf>"));
    }

    /// The marker is the user's file: a `mirror-of` edited into markup is not
    /// written back into `<mirrorOf>`.
    #[test]
    fn an_edited_mirror_of_is_not_written_back() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Maven]);
        let regs = vec![reg("m", "maven", "proxy")];
        let settings = dir.path().join(".m2/settings.xml");
        apply_all(&plan(&regs, &opts(), &env).unwrap());
        let forged = std::fs::read_to_string(&settings).unwrap().replace(
            "mirror-of=central",
            "mirror-of=central</mirrorOf><url>http://evil/</url><mirrorOf>x",
        );
        std::fs::write(&settings, forged).unwrap();

        apply_all(&plan(&regs, &opts(), &env).unwrap());
        let written = std::fs::read_to_string(&settings).unwrap();
        assert!(!written.contains("evil"), "{written}");
        assert!(
            written.contains("<mirrorOf>central</mirrorOf>"),
            "{written}"
        );
    }

    #[test]
    fn a_user_settings_xml_is_a_conflict() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Maven]);
        std::fs::create_dir_all(dir.path().join(".m2")).unwrap();
        std::fs::write(dir.path().join(".m2/settings.xml"), "<settings/>").unwrap();
        let p = plan(&[reg("m", "maven", "proxy")], &opts(), &env).unwrap();
        assert_eq!(find(&p, TargetId::Maven).state, State::Conflict);
        assert!(find(&p, TargetId::Maven)
            .snippet
            .as_deref()
            .unwrap()
            .contains("maven2"));
    }

    #[test]
    fn conflicts_in_the_file() {
        let cases: &[(ClientId, &str, &str, &str, &str)] = &[
            (
                ClientId::Npm,
                ".npmrc",
                "registry = https://registry.npmjs.org/\n",
                "@acme:registry=https://x/\n",
                "npm",
            ),
            (
                ClientId::Yarn,
                ".yarnrc.yml",
                "npmRegistryServer: \"https://x\"\n",
                "npmScopes:\n  acme:\n    npmRegistryServer: x\n",
                "npm",
            ),
            (
                ClientId::Pip,
                ".config/pip/pip.conf",
                "[global]\ntimeout=1\n",
                "[install]\nuser=true\n",
                "pypi",
            ),
            (
                ClientId::Uv,
                ".config/uv/uv.toml",
                "[[index]]\nurl = \"x\"\ndefault = true\n",
                "native-tls = true\n",
                "pypi",
            ),
            (
                ClientId::Go,
                ".config/go/env",
                "GOPROXY=https://proxy.golang.org\n",
                "GOPRIVATE=x\n",
                "goproxy",
            ),
            (
                ClientId::Cargo,
                ".cargo/config.toml",
                "[source.crates-io]\nreplace-with = \"x\"\n",
                "[net]\nretry = 3\n",
                "cargo",
            ),
        ];
        for (client, file, bad, fine, ty) in cases {
            for (content, want) in [(bad, State::Conflict), (fine, State::Absent)] {
                let dir = TempDir::new().unwrap();
                let env = env_in(dir.path(), &[*client]);
                let path = dir.path().join(file);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, content).unwrap();
                let plans = plan(&[reg("r", ty, "proxy")], &opts(), &env).unwrap();
                let p = plans.iter().find(|p| p.clients.contains(client)).unwrap();
                assert_eq!(p.state, want, "{client:?} with {content:?}: {:?}", p.detail);
            }
        }
    }

    /// yarn 4 refuses a plain-HTTP registry it was not told to trust
    /// (YN0081, measured with yarn 4.18.1 by the heavy suite).
    #[test]
    fn yarn_trusts_a_plain_http_registry_and_a_user_allow_list_conflicts() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Yarn]);
        let mut o = opts();
        o.server = "http://127.0.0.1:8181";
        let regs = vec![reg("n", "npm", "proxy")];
        let plans = plan(&regs, &o, &env).unwrap();
        let next = find(&plans, TargetId::Yarnrc).next.clone().unwrap();
        assert!(
            next.contains("unsafeHttpWhitelist:\n  - \"127.0.0.1\""),
            "{next}"
        );
        // Over https there is nothing to allow.
        let plans = plan(&regs, &opts(), &env).unwrap();
        assert!(!find(&plans, TargetId::Yarnrc)
            .next
            .clone()
            .unwrap()
            .contains("unsafeHttpWhitelist"));

        std::fs::write(
            dir.path().join(".yarnrc.yml"),
            "unsafeHttpWhitelist:\n  - \"x\"\n",
        )
        .unwrap();
        let plans = plan(&regs, &o, &env).unwrap();
        assert_eq!(find(&plans, TargetId::Yarnrc).state, State::Conflict);
    }

    #[test]
    fn an_environment_override_is_a_conflict() {
        let dir = TempDir::new().unwrap();
        let mut env = env_in(dir.path(), &[ClientId::Go]);
        env.vars
            .insert("GOPROXY".into(), "https://proxy.golang.org".into());
        let p = plan(&[reg("g", "goproxy", "proxy")], &opts(), &env).unwrap();
        assert_eq!(find(&p, TargetId::Go).state, State::Conflict);
        assert!(find(&p, TargetId::Go)
            .detail
            .as_deref()
            .unwrap()
            .contains("$GOPROXY"));

        // The same, exported from the managed rc file.
        let env = env_in(dir.path(), &[ClientId::Go]);
        std::fs::write(dir.path().join(".bashrc"), "export GOPROXY=https://x\n").unwrap();
        let p = plan(&[reg("g", "goproxy", "proxy")], &opts(), &env).unwrap();
        assert_eq!(find(&p, TargetId::Go).state, State::Conflict);
    }

    #[test]
    fn env_clients_write_env_sh_and_the_rc_block() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Rustup]);
        let regs = vec![
            reg("rust", "rustup", "proxy"),
            reg("node", "nodedist", "proxy"),
        ];
        let plans = plan(&regs, &opts(), &env).unwrap();
        let sh = find(&plans, TargetId::EnvSh);
        assert_eq!(sh.state, State::Absent);
        let body = sh.next.as_deref().unwrap();
        assert!(
            body.contains("export RUSTUP_DIST_SERVER='https://hub.example.com/proxy/rust/rustup'")
        );
        assert!(
            body.contains("export NODEJS_ORG_MIRROR='https://hub.example.com/proxy/node/nodedist'")
        );
        assert_eq!(find(&plans, TargetId::ShellRc).state, State::Absent);

        let mut none = opts();
        none.shell_rc = ShellRc::None;
        let plans = plan(&regs, &none, &env).unwrap();
        assert!(
            plans.iter().all(|p| p.target != TargetId::ShellRc),
            "--shell-rc none leaves rc files out of the plan"
        );
    }

    #[test]
    fn foreign_and_plain_http_hosts_are_refused() {
        let mut r = reg("n", "npm", "proxy");
        let mut foreign = opts();
        foreign.allow_foreign_hosts = true;
        r.public_url = Some("https://npm.hub.example.com".into());
        assert!(base_url(&r, &opts()).is_ok());
        r.public_url = Some("https://evil.example.org".into());
        assert!(base_url(&r, &opts()).is_err());
        assert!(base_url(&r, &foreign).is_ok());
        // `xhub.example.com` is not a subdomain.
        r.public_url = Some("https://xhub.example.com".into());
        assert!(base_url(&r, &opts()).is_err());

        assert!(check_server_scheme("http://hub.example.com", false).is_err());
        assert!(check_server_scheme("http://hub.example.com", true).is_ok());
        assert!(check_server_scheme("http://localhost:8080", false).is_ok());
        assert!(check_server_scheme("https://hub.example.com", false).is_ok());
    }

    #[test]
    fn rendered_blocks() {
        let base = "https://hub.example.com/proxy/r";
        assert_eq!(
            render_body(TargetId::Cargo, base),
            "[source.crates-io]\nreplace-with = \"batlehub\"\n\n[source.batlehub]\n\
             registry = \"sparse+https://hub.example.com/proxy/r/registry/\"\n\n\
             [registries.batlehub]\nindex = \"sparse+https://hub.example.com/proxy/r/registry/\""
        );
        assert_eq!(
            render_body(TargetId::Npmrc, base),
            "registry=https://hub.example.com/proxy/r/"
        );
        assert_eq!(
            render_body(TargetId::Yarnrc, base),
            "npmRegistryServer: \"https://hub.example.com/proxy/r/\""
        );
        assert_eq!(
            render_body(TargetId::Pip, base),
            "[global]\nindex-url = https://hub.example.com/proxy/r/simple/"
        );
        assert_eq!(
            render_body(TargetId::Uv, base),
            "[[index]]\nname = \"batlehub\"\nurl = \"https://hub.example.com/proxy/r/simple/\"\ndefault = true"
        );
        assert_eq!(
            render_body(TargetId::Go, base),
            "GOPROXY=https://hub.example.com/proxy/r"
        );
        assert!(render_maven("r", base, "central")
            .contains("<url>https://hub.example.com/proxy/r/maven2/</url>"));
    }

    #[test]
    fn written_toml_parses() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Cargo, ClientId::Uv, ClientId::Mise]);
        std::fs::create_dir_all(dir.path().join(".cargo")).unwrap();
        std::fs::write(dir.path().join(".cargo/config.toml"), "[net]\nretry = 3").unwrap();
        let regs = vec![
            reg("c", "cargo", "proxy"),
            reg("p", "pypi", "proxy"),
            reg("gh", "github", "proxy"),
        ];
        let plans = plan(&regs, &opts(), &env).unwrap();
        for id in [TargetId::Cargo, TargetId::Uv, TargetId::Mise] {
            let next = find(&plans, id).next.as_deref().unwrap();
            toml::from_str::<toml::Table>(next).unwrap_or_else(|e| panic!("{id:?}: {e}\n{next}"));
        }
    }

    #[test]
    fn remove_restores_every_file() {
        let dir = TempDir::new().unwrap();
        let env = env_in(
            dir.path(),
            &[
                ClientId::Npm,
                ClientId::Cargo,
                ClientId::Maven,
                ClientId::Rustup,
                ClientId::Mise,
            ],
        );
        std::fs::write(dir.path().join(".npmrc"), "save-exact=true").unwrap();
        std::fs::write(dir.path().join(".bashrc"), "alias ll='ls -l'\n").unwrap();
        let regs = vec![
            reg("n", "npm", "proxy"),
            reg("c", "cargo", "proxy"),
            reg("m", "maven", "proxy"),
            reg("r", "rustup", "proxy"),
            reg("gh", "github", "proxy"),
        ];
        apply_all(&plan(&regs, &opts(), &env).unwrap());
        assert!(dir.path().join(".m2/settings.xml").exists());

        for r in removals(&env, &[], &[], None).0 {
            remove_one(&r).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".npmrc")).unwrap(),
            "save-exact=true"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".bashrc")).unwrap(),
            "alias ll='ls -l'\n"
        );
        for gone in [
            ".m2/settings.xml",
            ".cargo/config.toml",
            ".config/batlehub/env.sh",
            ".config/mise/conf.d/batlehub.toml",
        ] {
            assert!(!dir.path().join(gone).exists(), "{gone}");
        }
        assert!(removals(&env, &[], &[], None).0.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn writes_go_through_a_symlink_and_keep_the_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let real = dir.path().join("dotfiles-npmrc");
        std::fs::write(&real, "a=1\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join(".npmrc");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        write_atomic(&link, b"a=2\n").unwrap();
        assert!(link.is_symlink());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "a=2\n");
        assert_eq!(
            std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn output_within_reads_stdout_and_kills_a_hang() {
        let mut ok = Command::new("sh");
        ok.args(["-c", "echo 4.18.1"]);
        assert_eq!(
            output_within(&mut ok, Duration::from_secs(5)).as_deref(),
            Some("4.18.1\n")
        );
        let mut fails = Command::new("sh");
        fails.args(["-c", "exit 1"]);
        assert_eq!(output_within(&mut fails, Duration::from_secs(5)), None);
        let mut hangs = Command::new("sleep");
        hangs.arg("5");
        let started = Instant::now();
        assert_eq!(output_within(&mut hangs, Duration::from_millis(100)), None);
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "the hung child was waited out"
        );
        assert_eq!(
            output_within(
                &mut Command::new("/nonexistent/bin"),
                Duration::from_secs(1)
            ),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn on_path_needs_an_executable_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let bin = dir.path().join("npm");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        assert!(!on_path("npm", &path), "not executable yet");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(on_path("npm", &path));
        assert!(!on_path("cargo", &path));
        std::fs::create_dir(dir.path().join("go")).unwrap();
        assert!(!on_path("go", &path), "a directory is not a binary");
    }

    #[test]
    fn env_from_process_always_has_node() {
        let env = Env::from_process(true).unwrap();
        assert!(env.installed.contains(&ClientId::Node));
        assert!(env.config_dir.is_absolute());
        // yarn is kept only when it could say its version.
        assert_eq!(
            env.installed.contains(&ClientId::Yarn),
            env.yarn_major.is_some()
        );
    }

    #[test]
    fn pip_dir_is_the_xdg_one_off_macos() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("Library/Application Support/pip")).unwrap();
        let got = pip_dir(dir.path(), &dir.path().join(".config"));
        if cfg!(target_os = "macos") {
            assert!(got.ends_with("Library/Application Support/pip"));
        } else {
            assert_eq!(got, dir.path().join(".config/pip"));
        }
    }

    #[test]
    fn parse_picks_takes_type_equals_name() {
        let picks = parse_picks(&["npm=a".to_owned(), "cargo=b".to_owned()]).unwrap();
        assert_eq!(picks.get("npm").map(String::as_str), Some("a"));
        for bad in ["npm", "=a", "npm="] {
            assert!(parse_picks(&[bad.to_owned()]).is_err(), "{bad}");
        }
        let regs = vec![reg("a", "npm", "proxy")];
        let picks = parse_picks(&["npm=nope".to_owned()]).unwrap();
        assert!(resolve("npm", &regs, &picks, None, &BTreeSet::new()).is_err());
    }

    #[test]
    fn a_fish_shell_gets_the_conf_d_file_and_no_rc_block() {
        let dir = TempDir::new().unwrap();
        let mut env = env_in(dir.path(), &[ClientId::Rustup]);
        env.shell = Some("fish".to_owned());
        let plans = plan(&[reg("r", "rustup", "proxy")], &opts(), &env).unwrap();
        let fish = find(&plans, TargetId::Fish);
        assert!(fish
            .next
            .as_deref()
            .unwrap()
            .contains("set -gx RUSTUP_DIST_SERVER 'https://hub.example.com/proxy/r/rustup'"));
        assert!(
            plans.iter().all(|p| p.target != TargetId::ShellRc),
            "fish has no rc block"
        );
    }

    #[test]
    fn diff_shows_only_the_span() {
        let d = diff(Path::new("f"), "a\nb\n", Some("a\nx\nb\n"));
        assert!(d.contains("+x\n") && !d.contains("-a") && !d.contains("+a"));
    }

    // ── Regressions of the PR #243 review (tmp/code-review-pr-243.md) ──

    fn npm_block(dir: &Path) -> String {
        std::fs::read_to_string(dir.join(".npmrc")).unwrap()
    }

    /// n° 1: `remove` never glues the line above the block to the one below.
    #[test]
    fn unsplice_never_glues_two_user_lines() {
        let m = Marker::default();
        let s = splice(
            Some("save-exact=true"),
            Comment::Hash,
            m.clone(),
            "x=1",
            false,
        )
        .unwrap();
        let edited = format!("{s}fund=false\n");
        assert_eq!(
            unsplice(&edited, Comment::Hash).unwrap().unwrap(),
            "save-exact=true\nfund=false\n"
        );
        // A file apply created, written above and below by the user since.
        let s = splice(None, Comment::Hash, m, "x=1", false).unwrap();
        let edited = format!("a=1\n{s}b=2\n");
        assert_eq!(
            unsplice(&edited, Comment::Hash).unwrap().unwrap(),
            "a=1\nb=2\n"
        );
    }

    /// n° 3: the URL written is the string checked, not what a parser makes
    /// of it; names are refused outside a safe set.
    #[test]
    fn base_url_refuses_what_a_config_file_would_misread() {
        let mut o = opts();
        o.server = "http://127.0.0.1:8080";
        let mut r = reg("n", "npm", "proxy");
        for bad in [
            "http://evil.example\n@127.0.0.1:8080",
            "http://127.0.0.1:8080/x\nscript-shell=/tmp/evil",
            "http://127.0.0.1:8080/'x",
            "http://user:pw@127.0.0.1:8080",
            "ftp://127.0.0.1:8080",
            "http://127.0.0.1:8080/?q=1",
        ] {
            r.public_url = Some(bad.into());
            assert!(base_url(&r, &o).is_err(), "{bad:?}");
        }
        // Plain HTTP for a public_url only on loopback or --insecure-http.
        r.public_url = Some("http://npm.hub.example.com".into());
        assert!(base_url(&r, &opts()).is_err());
        let mut insecure = opts();
        insecure.insecure_http = true;
        assert!(base_url(&r, &insecure).is_ok());
        let bad_name = reg("a b", "npm", "proxy");
        assert!(base_url(&bad_name, &opts()).is_err());
        assert!(check_server_scheme("http://127.0.0.1:1\n", false).is_err());

        // A maven name with `--` would break the XML comment: a conflict, and
        // the other clients are still planned.
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Maven, ClientId::Npm]);
        let regs = vec![
            reg("maven--central", "maven", "proxy"),
            reg("n", "npm", "proxy"),
        ];
        let plans = plan(&regs, &opts(), &env).unwrap();
        assert_eq!(find(&plans, TargetId::Maven).state, State::Conflict);
        assert_eq!(find(&plans, TargetId::Npmrc).state, State::Absent);
    }

    /// n° 4 (a): the value our own env.sh exported is not the user's.
    #[test]
    fn our_own_export_is_not_an_override() {
        let dir = TempDir::new().unwrap();
        let mut env = env_in(dir.path(), &[ClientId::Rustup]);
        apply_all(&plan(&[reg("old", "rustup", "proxy")], &opts(), &env).unwrap());
        env.vars.insert(
            "RUSTUP_DIST_SERVER".into(),
            "https://hub.example.com/proxy/old/rustup".into(),
        );
        let plans = plan(&[reg("new", "rustup", "proxy")], &opts(), &env).unwrap();
        assert_eq!(find(&plans, TargetId::EnvSh).state, State::Drifted);
    }

    /// n° 4 (b): no rc line sources an env.sh that was not written.
    #[test]
    fn an_env_sh_conflict_blocks_the_rc_block_and_the_fish_file() {
        for shell in ["bash", "fish"] {
            let dir = TempDir::new().unwrap();
            let mut env = env_in(dir.path(), &[ClientId::Rustup]);
            env.shell = Some(shell.to_owned());
            env.vars
                .insert("RUSTUP_DIST_SERVER".into(), "https://mirror.corp".into());
            let plans = plan(&[reg("r", "rustup", "proxy")], &opts(), &env).unwrap();
            assert_eq!(find(&plans, TargetId::EnvSh).state, State::Conflict);
            let shell_target = if shell == "fish" {
                TargetId::Fish
            } else {
                TargetId::ShellRc
            };
            let p = find(&plans, shell_target);
            assert_eq!(p.state, State::Conflict, "{shell}");
            assert!(!p.writes());
        }
    }

    /// n° 5: the bare line — npm dropped the markers, or the user pasted it —
    /// is up to date and never taken over; `remove` points it out.
    #[test]
    fn a_bare_registry_line_is_up_to_date_and_left_alone() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Npm]);
        let regs = vec![reg("n", "npm", "proxy")];
        std::fs::write(
            dir.path().join(".npmrc"),
            "registry=https://hub.example.com/proxy/n/\nfund=false\n",
        )
        .unwrap();
        let plans = plan(&regs, &opts(), &env).unwrap();
        let p = find(&plans, TargetId::Npmrc);
        assert_eq!(p.state, State::UpToDate, "{:?}", p.detail);
        assert!(!p.writes());

        // remove cannot tell our bare line from the user's without the
        // registry list: it points it out and edits nothing.
        let bare = "registry=https://hub.example.com/proxy/n/\nfund=false\n";
        std::fs::write(dir.path().join(".npmrc"), bare).unwrap();
        assert!(removals(&env, &[], &[], None).0.is_empty());
        let (r, warnings) = removals(&env, &[], &[], Some(SERVER));
        assert!(r.is_empty());
        assert!(warnings[0].contains("no batlehub markers"), "{warnings:?}");

        // A registry= the user set — another host, another registry or
        // another port of the same server — is theirs: conflict as before.
        for theirs in [
            "registry=https://registry.npmjs.org/\n",
            "registry=https://hub.example.com/proxy/mine/\n",
            "registry=https://hub.example.com:4873/\n",
        ] {
            std::fs::write(dir.path().join(".npmrc"), theirs).unwrap();
            let p = find(&plan(&regs, &opts(), &env).unwrap(), TargetId::Npmrc).clone();
            assert_eq!(p.state, State::Conflict, "{theirs}");
            assert!(
                removals(&env, &[], &[], Some(SERVER)).0.is_empty(),
                "{theirs}"
            );
        }
        // Only the pasted line: apply then remove leaves the file as it was.
        let pasted = "registry=https://hub.example.com/proxy/n/\n";
        std::fs::write(dir.path().join(".npmrc"), pasted).unwrap();
        apply_all(&plan(&regs, &opts(), &env).unwrap());
        assert!(removals(&env, &[], &[], None).0.is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".npmrc")).unwrap(),
            pasted
        );
    }

    /// n° 5 (go env -w) and n° 8 (settings.xml): an edited block or drop-in
    /// is left alone by apply, and an edited drop-in by remove.
    #[test]
    fn an_edited_block_or_drop_in_is_left_alone() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Go, ClientId::Maven]);
        let regs = vec![reg("g", "goproxy", "proxy"), reg("m", "maven", "proxy")];
        apply_all(&plan(&regs, &opts(), &env).unwrap());

        let goenv = dir.path().join(".config/go/env");
        let edited = std::fs::read_to_string(&goenv).unwrap().replace(
            "GOPROXY=https://hub.example.com/proxy/g",
            "GOPROXY=https://corp",
        );
        std::fs::write(&goenv, &edited).unwrap();
        let settings = dir.path().join(".m2/settings.xml");
        let xml = std::fs::read_to_string(&settings).unwrap().replace(
            "</settings>",
            "  <servers><server><id>x</id></server></servers>\n</settings>",
        );
        std::fs::write(&settings, &xml).unwrap();

        let plans = plan(&regs, &opts(), &env).unwrap();
        for id in [TargetId::Go, TargetId::Maven] {
            let p = find(&plans, id);
            assert_eq!(p.state, State::Conflict, "{id:?}");
            assert!(
                p.detail.as_deref().unwrap().contains("edited"),
                "{:?}",
                p.detail
            );
        }
        apply_all(&plans);
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), xml);

        let (r, warnings) = removals(&env, &[], &[], None);
        // remove leaves both: the user's GOPROXY and <servers> survive it.
        assert!(
            r.is_empty(),
            "{:?}",
            r.iter().map(|r| &r.path).collect::<Vec<_>>()
        );
        for f in ["settings.xml", "go/env"] {
            assert!(warnings.iter().any(|w| w.contains(f)), "{f}: {warnings:?}");
        }
    }

    /// n° 6: remove finds what a probe cannot see — an rc block with no
    /// `$SHELL`, go's env file with no go.
    #[test]
    fn remove_finds_blocks_whatever_the_shell_and_go() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Rustup, ClientId::Go]);
        let regs = vec![reg("r", "rustup", "proxy"), reg("g", "goproxy", "proxy")];
        apply_all(&plan(&regs, &opts(), &env).unwrap());
        let mut hook = env_in(dir.path(), &[]);
        hook.shell = None;
        let (r, _) = removals(&hook, &[], &[], None);
        for f in [".bashrc", ".config/go/env", ".config/batlehub/env.sh"] {
            assert!(r.iter().any(|r| r.path == dir.path().join(f)), "{f}");
        }
        let vars = BTreeMap::from([("GOENV".to_owned(), "off".to_owned())]);
        assert_eq!(go_env_path(dir.path(), dir.path(), &vars), None);
        assert_eq!(
            go_env_path(dir.path(), &dir.path().join(".config"), &BTreeMap::new()),
            Some(if cfg!(target_os = "macos") {
                dir.path().join("Library/Application Support/go/env")
            } else {
                dir.path().join(".config/go/env")
            })
        );
    }

    /// n° 7: what one run's options leave out is not orphaned.
    #[test]
    fn a_run_s_options_do_not_orphan_what_they_leave_out() {
        let dir = TempDir::new().unwrap();
        let mut env = env_in(dir.path(), &[ClientId::Rustup, ClientId::Npm]);
        env.shell = Some("fish".into());
        let regs = vec![
            reg("r", "rustup", "proxy"),
            reg("node", "nodedist", "proxy"),
            reg("n", "npm", "proxy"),
        ];
        apply_all(&plan(&regs, &opts(), &env).unwrap());

        // A hook with $SHELL unset keeps the fish drop-in up to date.
        env.shell = None;
        let plans = plan(&regs, &opts(), &env).unwrap();
        assert_eq!(find(&plans, TargetId::Fish).state, State::UpToDate);

        // --only rustup keeps the node variables in env.sh.
        let mut only = opts();
        only.only = vec![ClientId::Rustup];
        let p = find(&plan(&regs, &only, &env).unwrap(), TargetId::EnvSh).state;
        assert_eq!(p, State::UpToDate);

        // --skip npm leaves ~/.npmrc, shared with pnpm, alone.
        let (r, _) = removals(&env, &[], &[ClientId::Npm, ClientId::Rustup], None);
        assert!(
            r.is_empty(),
            "{:?}",
            r.iter().map(|r| &r.path).collect::<Vec<_>>()
        );
        let mut skip = opts();
        skip.skip = vec![ClientId::Npm];
        assert!(plan(&regs, &skip, &env)
            .unwrap()
            .iter()
            .all(|p| p.target != TargetId::Npmrc));
    }

    /// n° 9: the spliced TOML is parsed before it is written.
    #[test]
    fn a_splice_that_would_break_the_toml_is_a_conflict() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Cargo]);
        std::fs::create_dir_all(dir.path().join(".cargo")).unwrap();
        std::fs::write(
            dir.path().join(".cargo/config.toml"),
            "registries = { internal = { index = \"sparse+https://x/\" } }\n",
        )
        .unwrap();
        let p = plan(&[reg("c", "cargo", "proxy")], &opts(), &env).unwrap();
        assert_eq!(find(&p, TargetId::Cargo).state, State::Conflict);
    }

    /// n° 10: no temp file is left behind, and each write has its own.
    #[test]
    fn write_atomic_leaves_no_temp_file() {
        let dir = TempDir::new().unwrap();
        let f = dir.path().join(".npmrc");
        std::thread::scope(|s| {
            for i in 0..8 {
                let f = &f;
                s.spawn(move || write_atomic(f, format!("a={i}\n").as_bytes()).unwrap());
            }
        });
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(names.len(), 1, "{names:?}");
        assert!(std::fs::read_to_string(&f).unwrap().starts_with("a="));
    }

    /// n° 12: mise gets its node mirror, and a global setting that would
    /// silently replace the drop-in's is a conflict.
    #[test]
    fn mise_node_mirror_and_global_overrides() {
        let dir = TempDir::new().unwrap();
        let mut env = env_in(dir.path(), &[ClientId::Mise]);
        let regs = vec![
            reg("gh", "github", "proxy"),
            reg("node", "nodedist", "proxy"),
        ];
        let plans = plan(&regs, &opts(), &env).unwrap();
        let next = find(&plans, TargetId::Mise).next.clone().unwrap();
        assert!(
            next.contains(
                "[settings.node]\nmirror_url = \"https://hub.example.com/proxy/node/nodedist/\""
            ),
            "{next}"
        );
        toml::from_str::<toml::Table>(&next).unwrap();

        std::fs::create_dir_all(dir.path().join(".config/mise")).unwrap();
        std::fs::write(
            dir.path().join(".config/mise/config.toml"),
            "[settings.url_replacements]\n\"github.com\" = \"x\"\n",
        )
        .unwrap();
        assert_eq!(
            find(&plan(&regs, &opts(), &env).unwrap(), TargetId::Mise).state,
            State::Conflict
        );
        std::fs::remove_file(dir.path().join(".config/mise/config.toml")).unwrap();
        // Another conf.d file or config.local.toml: one side would be dropped.
        for other in ["conf.d/acme.toml", "conf.d/zz.toml", "config.local.toml"] {
            let f = dir.path().join(".config/mise").join(other);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(&f, "[settings.node]\nmirror_url = \"https://x/\"\n").unwrap();
            let p = find(&plan(&regs, &opts(), &env).unwrap(), TargetId::Mise).clone();
            assert_eq!(p.state, State::Conflict, "{other}");
            assert!(p.detail.unwrap().contains(other), "{other}");
            std::fs::remove_file(&f).unwrap();
        }
        env.vars.insert("MISE_URL_REPLACEMENTS".into(), "{}".into());
        assert_eq!(
            find(&plan(&regs, &opts(), &env).unwrap(), TargetId::Mise).state,
            State::Conflict
        );
    }

    /// n° 13: overrides are the ones that replace the block, no more, no less.
    #[test]
    fn overrides_are_the_ones_that_replace_the_block() {
        let cases: &[(ClientId, &str, &str, State)] = &[
            (
                ClientId::Cargo,
                "CARGO_REGISTRIES_MYCO_TOKEN",
                "cargo",
                State::Absent,
            ),
            (
                ClientId::Cargo,
                "CARGO_REGISTRIES_BATLEHUB_TOKEN",
                "cargo",
                State::Absent,
            ),
            (
                ClientId::Cargo,
                "CARGO_SOURCE_CRATES_IO_REPLACE_WITH",
                "cargo",
                State::Conflict,
            ),
            (
                ClientId::Npm,
                "NPM_CONFIG_USERCONFIG",
                "npm",
                State::Conflict,
            ),
            (ClientId::Npm, "Npm_Config_Registry", "npm", State::Conflict),
            (ClientId::Uv, "UV_INDEX", "pypi", State::Conflict),
            (ClientId::Uv, "UV_NO_CONFIG", "pypi", State::Conflict),
        ];
        for (client, var, ty, want) in cases {
            let dir = TempDir::new().unwrap();
            let mut env = env_in(dir.path(), &[*client]);
            env.vars.insert((*var).into(), "1".into());
            let plans = plan(&[reg("r", ty, "proxy")], &opts(), &env).unwrap();
            let p = plans.iter().find(|p| p.clients.contains(client)).unwrap();
            assert_eq!(p.state, *want, "{var}: {:?}", p.detail);
        }
    }

    /// n° 14: the registry a block names is gone — never left on its dead URL.
    #[test]
    fn a_gone_registry_orphans_or_switches_visibly() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Npm]);
        let mut picked = opts();
        picked.picks = parse_picks(&["npm=b".to_owned()]).unwrap();
        let abc = vec![
            reg("a", "npm", "proxy"),
            reg("b", "npm", "proxy"),
            reg("c", "npm", "proxy"),
        ];
        apply_all(&plan(&abc, &picked, &env).unwrap());

        let ac = vec![reg("a", "npm", "proxy"), reg("c", "npm", "proxy")];
        let plans = plan(&ac, &opts(), &env).unwrap();
        let p = find(&plans, TargetId::Npmrc);
        assert_eq!(p.state, State::Orphaned);
        assert!(
            p.detail.as_deref().unwrap().contains("b is gone"),
            "{:?}",
            p.detail
        );
        assert!(!p.writes(), "kept on its dead URL until remove or --pick");

        let a = vec![reg("a", "npm", "proxy")];
        let p = find(&plan(&a, &opts(), &env).unwrap(), TargetId::Npmrc).clone();
        assert_eq!(p.state, State::Drifted);
        assert!(
            p.detail.as_deref().unwrap().contains("switched to a"),
            "{:?}",
            p.detail
        );
    }

    /// n° 15: one broken file is that file's conflict; the rest is planned,
    /// and remove still removes the others.
    #[test]
    fn one_broken_file_does_not_stop_the_others() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Npm, ClientId::Rustup]);
        let regs = vec![reg("n", "npm", "proxy"), reg("r", "rustup", "proxy")];
        apply_all(&plan(&regs, &opts(), &env).unwrap());
        let broken: String = npm_block(dir.path())
            .lines()
            .filter(|l| !l.contains(MARKER_END))
            .map(|l| format!("{l}\n"))
            .collect();
        std::fs::write(dir.path().join(".npmrc"), broken).unwrap();

        let plans = plan(&regs, &opts(), &env).unwrap();
        let p = find(&plans, TargetId::Npmrc);
        assert_eq!(p.state, State::Conflict);
        assert!(
            !p.detail.as_deref().unwrap().contains("setup remove"),
            "{:?}",
            p.detail
        );
        assert_eq!(find(&plans, TargetId::EnvSh).state, State::UpToDate);

        let (r, warnings) = removals(&env, &[], &[], None);
        assert!(
            warnings.iter().any(|w| w.contains(".npmrc")),
            "{warnings:?}"
        );
        assert!(r.iter().any(|r| r.path.ends_with("env.sh")));
        assert_eq!(
            cached_env(&env).unwrap()[0],
            (
                "RUSTUP_DIST_SERVER".to_owned(),
                "https://hub.example.com/proxy/r/rustup".to_owned()
            )
        );
    }

    // ── Regressions of the second review loop ──

    /// pip rewrites pip.conf without comments too: the bare section is up
    /// to date and left alone, but not once a key of its own is under it.
    #[test]
    fn a_bare_pip_section_counts_whole_only() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Pip]);
        let regs = vec![reg("py", "pypi", "proxy")];
        let conf = dir.path().join(".config/pip/pip.conf");
        std::fs::create_dir_all(conf.parent().unwrap()).unwrap();
        let ours = "[global]\nindex-url = https://hub.example.com/proxy/py/simple/\n";
        std::fs::write(&conf, format!("{ours}\n[install]\nuser = false\n")).unwrap();
        let p = find(&plan(&regs, &opts(), &env).unwrap(), TargetId::Pip).clone();
        assert_eq!(p.state, State::UpToDate, "{:?}", p.detail);
        assert!(!p.writes());

        std::fs::write(&conf, format!("{ours}timeout = 5\n")).unwrap();
        assert_eq!(
            find(&plan(&regs, &opts(), &env).unwrap(), TargetId::Pip).state,
            State::Conflict
        );
    }

    /// The pick of one type settles nothing for another; a sibling file's
    /// pick of the same type does.
    #[test]
    fn a_pick_is_kept_for_its_type_only() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Npm, ClientId::Pip, ClientId::Yarn]);
        let regs = vec![
            reg("main", "npm", "proxy"),
            reg("other", "npm", "proxy"),
            reg("main", "pypi", "proxy"),
        ];
        let mut pip_only = opts();
        pip_only.only = vec![ClientId::Pip];
        apply_all(&plan(&regs, &pip_only, &env).unwrap());
        assert_eq!(
            find(&plan(&regs, &opts(), &env).unwrap(), TargetId::Npmrc).state,
            State::Ambiguous
        );
        let mut picked = opts();
        picked.picks = parse_picks(&["npm=other".to_owned()]).unwrap();
        picked.only = vec![ClientId::Yarn];
        apply_all(&plan(&regs, &picked, &env).unwrap());
        let p = find(&plan(&regs, &opts(), &env).unwrap(), TargetId::Npmrc).clone();
        assert_eq!(p.registries, ["other"], "{:?}", p.state);
    }

    /// A pick no selected client uses still has to name a real registry.
    #[test]
    fn an_unknown_pick_fails_even_when_unused() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Npm]);
        let mut o = opts();
        o.picks = parse_picks(&["cargo=bogus".to_owned()]).unwrap();
        assert!(plan(&[reg("n", "npm", "proxy")], &o, &env).is_err());
    }

    /// The shell still exports the env.sh of a registry since renamed: that
    /// value is this instance's, not the user's.
    #[test]
    fn a_stale_export_on_the_server_host_is_not_an_override() {
        let dir = TempDir::new().unwrap();
        let mut env = env_in(dir.path(), &[ClientId::Rustup]);
        apply_all(&plan(&[reg("r1", "rustup", "proxy")], &opts(), &env).unwrap());
        apply_all(&plan(&[reg("r2", "rustup", "proxy")], &opts(), &env).unwrap());
        env.vars.insert(
            "RUSTUP_DIST_SERVER".into(),
            "https://hub.example.com/proxy/r1/rustup".into(),
        );
        let plans = plan(&[reg("r2", "rustup", "proxy")], &opts(), &env).unwrap();
        assert_eq!(find(&plans, TargetId::EnvSh).state, State::UpToDate);
        assert_eq!(find(&plans, TargetId::ShellRc).state, State::UpToDate);
    }

    /// A latin-1 `~/.bashrc` gets its block, and keeps every byte.
    #[test]
    fn a_file_that_is_not_utf8_keeps_its_bytes() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Rustup]);
        let original: &[u8] = b"# caf\xe9\nalias ll='ls -l'\n";
        std::fs::write(dir.path().join(".bashrc"), original).unwrap();
        let plans = plan(&[reg("r", "rustup", "proxy")], &opts(), &env).unwrap();
        assert_eq!(find(&plans, TargetId::ShellRc).state, State::Absent);
        apply_all(&plans);
        let written = std::fs::read(dir.path().join(".bashrc")).unwrap();
        assert!(written.ends_with(original), "{written:?}");
        assert_eq!(
            find(
                &plan(&[reg("r", "rustup", "proxy")], &opts(), &env).unwrap(),
                TargetId::ShellRc
            )
            .state,
            State::UpToDate
        );
        for r in removals(&env, &[], &[], None).0 {
            remove_one(&r).unwrap();
        }
        assert_eq!(std::fs::read(dir.path().join(".bashrc")).unwrap(), original);
    }

    /// The `setup env` fallback is `eval`ed: an edited env.sh is not trusted.
    #[test]
    fn cached_env_trusts_only_an_unedited_file() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Rustup]);
        apply_all(&plan(&[reg("r", "rustup", "proxy")], &opts(), &env).unwrap());
        assert!(cached_env(&env).is_some());
        let path = env_sh_path(&env);
        let content = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{content}export X;id;:='a'\n")).unwrap();
        assert_eq!(cached_env(&env), None);
    }

    /// `setup env` prints env.sh without the server only when it is for that
    /// server and nothing in the process overrides it.
    #[test]
    fn cached_env_for_needs_the_same_server_and_no_override() {
        let dir = TempDir::new().unwrap();
        let mut env = env_in(dir.path(), &[ClientId::Rustup]);
        apply_all(&plan(&[reg("r", "rustup", "proxy")], &opts(), &env).unwrap());
        let usable = |c: Cached| matches!(c, Cached::Usable(_));
        assert!(usable(cached_env_for(
            &env,
            Some("https://hub.example.com")
        )));
        assert!(
            usable(cached_env_for(&env, None)),
            "no server: the file is the answer"
        );
        for other in ["https://other.example.com", "https://hub.example.com:8443"] {
            assert_eq!(cached_env_for(&env, Some(other)), Cached::Unusable);
        }
        let (k, v) = cached_env(&env).unwrap().remove(0);
        env.vars.insert(k.clone(), v);
        assert!(
            usable(cached_env_for(&env, Some("https://hub.example.com"))),
            "an earlier eval"
        );
        env.vars.insert(k.clone(), String::new());
        assert!(usable(cached_env_for(&env, None)), "empty is unset");
        env.vars.insert(k, "https://mirror.corp.example".to_owned());
        for server in [Some("https://hub.example.com"), None] {
            assert!(matches!(
                cached_env_for(&env, server),
                Cached::Overridden(_)
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_creates_its_target() {
        let dir = TempDir::new().unwrap();
        let link = dir.path().join(".npmrc");
        std::os::unix::fs::symlink("dotfiles/npmrc", &link).unwrap();
        std::fs::create_dir_all(dir.path().join("dotfiles")).unwrap();
        write_atomic(&link, b"a=1\n").unwrap();
        assert!(link.is_symlink());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("dotfiles/npmrc")).unwrap(),
            "a=1\n"
        );
    }

    #[test]
    fn find_run_matches_whole_lines_only() {
        assert_eq!(
            find_run("a=1\nregistry=x/\nb=2\n", "registry=x/"),
            Some((4, 16))
        );
        assert_eq!(find_run("registry=x/y\n", "registry=x/"), None);
        assert_eq!(find_run("registry=x/\r\n", "registry=x/"), Some((0, 13)));
        assert_eq!(
            find_run("[global]\nk = v\n[x]\n", "[global]\nk = v"),
            Some((0, 15))
        );
        assert_eq!(
            find_run("[global]\nk = v\nz = 1\n", "[global]\nk = v"),
            None
        );
        assert_eq!(find_run("", "x"), None);
    }

    #[test]
    fn a_comma_in_a_url_is_refused() {
        let mut r = reg("g", "goproxy", "proxy");
        r.public_url = Some("https://hub.example.com/p,https://evil.example".into());
        assert!(base_url(&r, &opts()).is_err());
    }

    // ── Regressions of the third review loop ──

    #[test]
    fn find_run_refuses_a_section_or_list_that_goes_on() {
        // configparser reads past a blank line; YAML continues a list.
        assert_eq!(
            find_run("[global]\nk = v\n\ntimeout = 60\n", "[global]\nk = v"),
            None
        );
        assert_eq!(
            find_run("[global]\nk = v\n\n# note\n[install]\n", "[global]\nk = v"),
            Some((0, 15))
        );
        assert_eq!(find_run("a:\n  - x\n  - y\n", "a:\n  - x"), None);
    }

    /// An index pip or uv asks besides, or over, the one the block writes is
    /// a conflict: a version the instance withholds would install from it.
    #[test]
    fn another_python_index_is_a_conflict() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Pip, ClientId::Uv]);
        let regs = [reg("py", "pypi", "proxy")];
        let state = |env: &Env, id| find(&plan(&regs, &opts(), env).unwrap(), id).state;
        assert_eq!(state(&env, TargetId::Pip), State::Absent);
        assert_eq!(state(&env, TargetId::Uv), State::Absent);

        let conf = dir.path().join(".config/pip/pip.conf");
        std::fs::create_dir_all(conf.parent().unwrap()).unwrap();
        std::fs::write(&conf, "[install]\nindex-url = https://corp/simple\n").unwrap();
        assert_eq!(state(&env, TargetId::Pip), State::Conflict);
        std::fs::remove_file(&conf).unwrap();

        let uv = dir.path().join(".config/uv/uv.toml");
        std::fs::create_dir_all(uv.parent().unwrap()).unwrap();
        std::fs::write(
            &uv,
            "[[index]]\nname = \"corp\"\nurl = \"https://corp/simple\"\n",
        )
        .unwrap();
        assert_eq!(state(&env, TargetId::Uv), State::Conflict);
        std::fs::remove_file(&uv).unwrap();

        for (var, id) in [
            ("PIP_EXTRA_INDEX_URL", TargetId::Pip),
            ("UV_EXTRA_INDEX_URL", TargetId::Uv),
        ] {
            let mut env = env.clone();
            env.vars
                .insert(var.into(), "https://pypi.org/simple".into());
            assert_eq!(state(&env, id), State::Conflict, "{var}");
        }
    }

    /// A setting that agrees with the block is not a conflict: the URL it
    /// writes, a config-file variable naming its own file, cargo's legacy
    /// `config` symlinked to `config.toml`.
    #[cfg(unix)]
    #[test]
    fn settings_that_agree_are_not_conflicts() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Go, ClientId::Npm, ClientId::Cargo]);
        let regs = [
            reg("g", "goproxy", "proxy"),
            reg("n", "npm", "proxy"),
            reg("c", "cargo", "proxy"),
        ];
        let state = |env: &Env, id| find(&plan(&regs, &opts(), env).unwrap(), id).state;
        let mut e = env.clone();
        e.vars
            .insert("GOPROXY".into(), "https://hub.example.com/proxy/g".into());
        e.vars.insert(
            "NPM_CONFIG_USERCONFIG".into(),
            dir.path().join(".npmrc").to_string_lossy().into_owned(),
        );
        assert_eq!(state(&e, TargetId::Go), State::Absent);
        assert_eq!(state(&e, TargetId::Npmrc), State::Absent);
        e.vars
            .insert("GOPROXY".into(), "https://proxy.golang.org".into());
        assert_eq!(state(&e, TargetId::Go), State::Conflict);

        let cargo = dir.path().join(".cargo");
        std::fs::create_dir_all(&cargo).unwrap();
        std::fs::write(cargo.join("config.toml"), "").unwrap();
        std::os::unix::fs::symlink("config.toml", cargo.join("config")).unwrap();
        assert_eq!(state(&env, TargetId::Cargo), State::Absent);
    }

    /// An exported but empty variable is unset; another service on the
    /// server's host is not the instance.
    #[test]
    fn env_sh_overrides_are_values_not_presence() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Rustup]);
        let regs = [reg("r", "rustup", "proxy")];
        let state = |env: &Env| find(&plan(&regs, &opts(), env).unwrap(), TargetId::EnvSh).state;
        let mut e = env.clone();
        e.vars.insert("NODEJS_ORG_MIRROR".into(), String::new());
        assert_eq!(state(&e), State::Absent);
        e.vars.insert(
            "RUSTUP_DIST_SERVER".into(),
            "https://hub.example.com:4873".into(),
        );
        assert_eq!(state(&e), State::Conflict);
    }

    #[test]
    fn the_rc_file_is_the_one_the_shell_reads() {
        let dir = TempDir::new().unwrap();
        let mut env = env_in(dir.path(), &[]);
        let h = dir.path();
        // XDG zsh: ~/.zshenv sets ZDOTDIR, and apply's own shell inherited it.
        env.vars.insert(
            "ZDOTDIR".into(),
            h.join(".config/zsh").to_string_lossy().into(),
        );
        assert_eq!(zshenv(&env), h.join(".config/zsh/.zshenv"));
        std::fs::write(h.join(".zshenv"), "export ZDOTDIR=\"$HOME/.config/zsh\"\n").unwrap();
        assert_eq!(zshenv(&env), h.join(".zshenv"));
        // macOS bash: the login file bash already reads, never a new one hiding it.
        assert_eq!(bash_rc(&env, false), h.join(".bashrc"));
        assert_eq!(bash_rc(&env, true), h.join(".bash_profile"));
        std::fs::write(h.join(".profile"), "").unwrap();
        assert_eq!(bash_rc(&env, true), h.join(".profile"));
    }

    #[test]
    fn remove_says_why_it_leaves_a_drop_in_whose_marker_moved() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Maven]);
        apply_all(&plan(&[reg("m", "maven", "proxy")], &opts(), &env).unwrap());
        let xml = dir.path().join(".m2/settings.xml");
        let edited = format!(
            "<?xml version=\"1.0\"?>\n{}",
            std::fs::read_to_string(&xml).unwrap()
        );
        std::fs::write(&xml, &edited).unwrap();
        let (r, warnings) = removals(&env, &[], &[], None);
        assert!(r.is_empty());
        assert!(
            warnings.iter().any(|w| w.contains("below its first line")),
            "{warnings:?}"
        );
    }

    /// yarn 1 globally still leaves ~/.yarnrc.yml managed, for the yarn 4 a
    /// project pins.
    #[test]
    fn yarn_1_keeps_the_yarnrc_yml_target() {
        let dir = TempDir::new().unwrap();
        let mut env = env_in(dir.path(), &[ClientId::Yarn]);
        env.yarn_major = Some(1);
        let plans = plan(&[reg("n", "npm", "proxy")], &opts(), &env).unwrap();
        assert_eq!(find(&plans, TargetId::Yarnrc).state, State::Absent);
        assert!(find(&plans, TargetId::Npmrc)
            .clients
            .contains(&ClientId::Yarn));
    }

    #[test]
    fn instance_urls_are_the_server_s_own_registries_only() {
        let s = "https://hub.example.com";
        assert!(is_instance_url("https://hub.example.com/proxy/r/rustup", s));
        assert!(is_instance_url("https://npm.hub.example.com/", s));
        assert!(!is_instance_url("https://hub.example.com:4873/", s));
        assert!(!is_instance_url("https://hub.example.com/other", s));
        assert!(!is_instance_url("http://hub.example.com/proxy/r/", s));
        let prefixed = "https://host.example.com/batlehub";
        assert!(is_instance_url(
            "https://host.example.com/batlehub/proxy/node",
            prefixed
        ));
        assert!(!is_instance_url(
            "https://host.example.com/proxy/node",
            prefixed
        ));
        assert!(!is_instance_url("https://evilhub.example.com/proxy/r/", s));
    }

    /// A plain-HTTP index needs `trusted-host`, or pip ignores it.
    #[test]
    fn a_plain_http_pip_index_is_trusted() {
        assert_eq!(
            render_body(TargetId::Pip, "http://10.0.0.7:8080/proxy/py"),
            "[global]\nindex-url = http://10.0.0.7:8080/proxy/py/simple/\ntrusted-host = 10.0.0.7"
        );
    }

    #[test]
    fn a_global_header_with_a_comment_is_a_conflict() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Pip]);
        let conf = dir.path().join(".config/pip/pip.conf");
        std::fs::create_dir_all(conf.parent().unwrap()).unwrap();
        std::fs::write(&conf, "[global] # mine\ntimeout = 60\n").unwrap();
        let p = plan(&[reg("py", "pypi", "proxy")], &opts(), &env).unwrap();
        assert_eq!(find(&p, TargetId::Pip).state, State::Conflict);
        // pip skips the user file when PIP_CONFIG_FILE names one that exists,
        // whatever it holds; a missing one changes nothing.
        std::fs::remove_file(&conf).unwrap();
        let other = dir.path().join("extra.conf");
        std::fs::write(&other, "[install]\nuser = true\n").unwrap();
        let mut env = env;
        env.vars.insert(
            "PIP_CONFIG_FILE".into(),
            other.to_string_lossy().into_owned(),
        );
        let p = plan(&[reg("py", "pypi", "proxy")], &opts(), &env).unwrap();
        assert_eq!(find(&p, TargetId::Pip).state, State::Conflict);
        std::fs::remove_file(&other).unwrap();
        let p = plan(&[reg("py", "pypi", "proxy")], &opts(), &env).unwrap();
        assert_eq!(find(&p, TargetId::Pip).state, State::Absent);
    }

    /// npm's variable exported from the rc file, in any case, and pnpm's own
    /// global rc both win over ~/.npmrc.
    #[test]
    fn npm_and_pnpm_overrides_outside_the_environment() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Npm, ClientId::Pnpm]);
        let regs = [reg("n", "npm", "proxy")];
        let state = || find(&plan(&regs, &opts(), &env).unwrap(), TargetId::Npmrc).state;
        assert_eq!(state(), State::Absent);
        std::fs::write(
            dir.path().join(".bashrc"),
            "export NPM_CONFIG_REGISTRY=https://corp.example/\n",
        )
        .unwrap();
        assert_eq!(state(), State::Conflict);
        std::fs::remove_file(dir.path().join(".bashrc")).unwrap();
        let pnpm = dir.path().join(".config/pnpm");
        std::fs::create_dir_all(&pnpm).unwrap();
        std::fs::write(pnpm.join("auth.ini"), "registry=https://corp.example/\n").unwrap();
        assert_eq!(state(), State::Conflict);
    }

    /// The rc block goes first: the stock `~/.bashrc` returns early for a
    /// non-interactive shell, and nothing appended below that is read.
    #[test]
    fn the_rc_block_goes_above_an_early_return() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Rustup]);
        let skel = "case $- in\n    *i*) ;;\n      *) return;;\nesac\nalias ll='ls -l'\n";
        std::fs::write(dir.path().join(".bashrc"), skel).unwrap();
        apply_all(&plan(&[reg("r", "rustup", "proxy")], &opts(), &env).unwrap());
        let rc = std::fs::read_to_string(dir.path().join(".bashrc")).unwrap();
        assert!(rc.starts_with("# >>> batlehub setup"), "{rc}");
        assert!(rc.ends_with(skel), "{rc}");
        assert!(rc.contains(r#". "$HOME/.config/batlehub/env.sh""#), "{rc}");
        for r in removals(&env, &[], &[], None).0 {
            remove_one(&r).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".bashrc")).unwrap(),
            skel
        );
    }

    #[test]
    fn zdotdir_and_mise_config_dir_are_followed() {
        let dir = TempDir::new().unwrap();
        let mut env = env_in(dir.path(), &[ClientId::Mise]);
        env.shell = Some("zsh".into());
        env.vars.insert(
            "ZDOTDIR".into(),
            dir.path().join("zsh").to_string_lossy().into_owned(),
        );
        env.vars.insert(
            "MISE_CONFIG_DIR".into(),
            dir.path().join("m").to_string_lossy().into_owned(),
        );
        assert_eq!(rc_path(&env), Some(dir.path().join("zsh/.zshenv")));
        assert_eq!(
            TargetId::Mise.target(&env).unwrap().path,
            dir.path().join("m/conf.d/batlehub.toml")
        );
        assert!(all_rc_paths(&env).contains(&dir.path().join("zsh/.zshenv")));
    }

    /// A forged env.sh with a recomputed sum: still only known names and
    /// plain URLs reach the `eval`.
    #[test]
    fn cached_env_prints_only_known_names_and_plain_urls() {
        let dir = TempDir::new().unwrap();
        let env = env_in(dir.path(), &[ClientId::Rustup]);
        let body = "export RUSTUP_X;id;:='x'\nexport RUSTUP_DIST_SERVER='https://a/$(id)'\n\
                    export RUSTUP_UPDATE_ROOT='https://hub.example.com/proxy/r/rustup/rustup'\n";
        let forged = render_drop_in(&Marker::default(), Comment::Hash, body);
        let path = env_sh_path(&env);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, forged).unwrap();
        assert_eq!(
            cached_env(&env).unwrap(),
            [(
                "RUSTUP_UPDATE_ROOT".to_owned(),
                "https://hub.example.com/proxy/r/rustup/rustup".to_owned()
            )]
        );
    }

    #[test]
    fn urls_are_written_as_checked_on_the_server_s_port() {
        let mut r = reg("..", "npm", "proxy");
        assert!(base_url(&r, &opts()).is_err(), "`..` climbs out of /proxy/");
        r = reg("n", "npm", "proxy");
        r.public_url = Some("https://hub.example.com:8443/x".into());
        assert!(base_url(&r, &opts()).is_err(), "another port");
        r.public_url = Some("https://１２７.0.0.1/x".into());
        assert!(base_url(&r, &opts()).is_err(), "not ASCII");
        let mut o = opts();
        o.server = "http://127.0.0.1:8080";
        r.public_url = Some("HTTP://0x7f.0.0.1:8080/a/../b".into());
        assert_eq!(base_url(&r, &o).unwrap(), "http://127.0.0.1:8080/b");
    }

    #[cfg(unix)]
    #[test]
    fn delete_takes_a_link_s_target_and_keeps_the_link() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("dot")).unwrap();
        let link = dir.path().join(".npmrc");
        std::os::unix::fs::symlink("dot/npmrc", &link).unwrap();
        write_atomic(&link, b"x\n").unwrap();
        delete(&link).unwrap();
        assert!(link.is_symlink(), "the user's link stays");
        assert!(!dir.path().join("dot/npmrc").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_file_is_left_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let f = dir.path().join(".npmrc");
        std::fs::write(&f, "a=1\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o444)).unwrap();
        assert!(write_atomic(&f, b"a=2\n").is_err());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "a=1\n");
    }
}
