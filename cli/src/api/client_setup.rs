//! `setup apply | status | remove | env` — writing each package manager's
//! user-level configuration from the server's registry list (RFC 0038).
//!
//! One function, [`plan`], decides everything; the verbs only print or write
//! what it returns. The invariant the module keeps: **nothing outside a marker
//! pair is ever modified**. [`splice`] and [`unsplice`] are the only code that
//! edits a file's content, they copy the bytes around the managed span
//! untouched, and the start marker records the one byte (`nl`) a splice had to
//! add — so `apply` then `remove` gives back the original file, byte for byte.
//!
//! There is no state file: the marker line carries the registry the block was
//! written for (the pick a later run keeps), whether `apply` created the file,
//! and Maven's `mirror-of`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;

use crate::api::registry::{host_of, RegistryInfo};
use crate::api::suggest::{push_replacement, typed_url_replacements};

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
    /// `go env GOENV`, when `go` is installed and answers.
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
    pub fn from_process() -> Result<Self> {
        let home =
            dirs::home_dir().ok_or_else(|| anyhow!("cannot determine the home directory"))?;
        let vars: BTreeMap<String, String> = std::env::vars().collect();
        let non_empty = |k: &str| vars.get(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let config_dir = non_empty("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
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
        let yarn_major = if installed.contains(&ClientId::Yarn) {
            let mut cmd = Command::new("yarn");
            cmd.arg("--version")
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

        let goenv = if installed.contains(&ClientId::Go) {
            let mut cmd = Command::new("go");
            cmd.args(["env", "GOENV"]);
            output_within(&mut cmd, Duration::from_secs(10))
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty() && s != "off")
                .map(PathBuf::from)
        } else {
            None
        };

        Ok(Self {
            pip_dir: pip_dir(&home, &config_dir),
            shell: vars
                .get("SHELL")
                .and_then(|s| Path::new(s).file_name())
                .map(|s| s.to_string_lossy().into_owned()),
            home,
            config_dir,
            cargo_home,
            goenv,
            vars,
            installed,
            yarn_major,
        })
    }
}

/// pip's user config directory. On macOS pip reads
/// `~/Library/Application Support/pip` *instead of* `~/.config/pip` when that
/// directory exists, so writing the other one would be ignored.
fn pip_dir(home: &Path, config_dir: &Path) -> PathBuf {
    let library = home.join("Library/Application Support/pip");
    if cfg!(target_os = "macos") && library.is_dir() {
        library
    } else {
        config_dir.join("pip")
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

const MARKER_START: &str = ">>> batlehub setup (managed — edit with `batlehub-cli setup`)";
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
                env.config_dir.join("mise/conf.d/batlehub.toml"),
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
            TargetId::Yarnrc if yarn_reads_npmrc => vec![],
            TargetId::Yarnrc => vec![ClientId::Yarn],
            TargetId::Pip => vec![ClientId::Pip],
            TargetId::Uv => vec![ClientId::Uv],
            TargetId::Go => vec![ClientId::Go],
            TargetId::Maven => vec![ClientId::Maven],
            TargetId::Mise => vec![ClientId::Mise],
            TargetId::EnvSh | TargetId::Fish | TargetId::ShellRc => {
                vec![ClientId::Rustup, ClientId::Node]
            }
        }
    }
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
        "bash" => Some(env.home.join(".bashrc")),
        "zsh" => Some(env.home.join(".zshenv")),
        _ => None,
    }
}

/// Both rc files a block may have been written to, whatever the shell is now —
/// `remove` must find a block written before the user changed shells.
fn all_rc_paths(env: &Env) -> [PathBuf; 2] {
    [env.home.join(".bashrc"), env.home.join(".zshenv")]
}

// ── Markers ───────────────────────────────────────────────────────────────────

/// The fields of a start marker — the command's only state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marker {
    /// The registries the block was written for; a later run keeps them as
    /// its picks.
    pub registries: Vec<String>,
    /// `apply` created the file, so `remove` deletes it once empty.
    pub created: bool,
    /// `apply` added a final newline the original lacked.
    pub nl: bool,
    /// Maven's `--maven-mirror-of`.
    pub mirror_of: Option<String>,
}

impl Marker {
    fn render(&self, comment: Comment) -> String {
        let mut s = format!("{MARKER_START} registry={}", self.registries.join(","));
        if self.created {
            s.push_str(" created");
        }
        if self.nl {
            s.push_str(" nl");
        }
        if let Some(m) = &self.mirror_of {
            s.push_str(&format!(" mirror-of={m}"));
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
                        .map(str::to_owned)
                        .collect();
                }
                Some(("mirror-of", v)) => m.mirror_of = Some(v.to_owned()),
                _ if field == "created" => m.created = true,
                _ if field == "nl" => m.nl = true,
                _ => {}
            }
        }
        Some(m)
    }
}

/// A managed block found in a file: byte offsets of its first and one-past-last
/// byte (end marker line included, with its newline).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Span {
    start: usize,
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
    let mut start: Option<(usize, Marker)> = None;
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
            start = Some((line_start, marker));
        } else if is_end_line(comment, line) {
            let (s, marker) = start
                .take()
                .ok_or_else(|| anyhow!("batlehub setup end marker without a start marker"))?;
            found = Some(Span {
                start: s,
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
    let mut s = marker.render(comment);
    s.push('\n');
    s.push_str(body.trim_end_matches('\n'));
    s.push('\n');
    s.push_str(&comment.wrap(MARKER_END));
    s.push('\n');
    s
}

/// `original` with the managed block set to `body`, and the marker of the
/// block written. An existing block is replaced in place and keeps its
/// `created`/`nl`; a new one is appended after a blank line.
fn splice(
    original: Option<&str>,
    comment: Comment,
    mut marker: Marker,
    body: &str,
) -> Result<String> {
    let content = original.unwrap_or("");
    if let Some(span) = find_block(content, comment)? {
        marker.created = span.marker.created;
        marker.nl = span.marker.nl;
        return Ok(format!(
            "{}{}{}",
            &content[..span.start],
            render_block(&marker, comment, body),
            &content[span.end..]
        ));
    }
    marker.created = original.is_none();
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

/// `content` with the managed block taken out, the blank line `splice` put
/// before it and the newline it added (`nl`) included. `None` when the file
/// should be deleted: `apply` created it and nothing else is left.
fn unsplice(content: &str, comment: Comment) -> Result<Option<String>> {
    let Some(span) = find_block(content, comment)? else {
        return Ok(Some(content.to_owned()));
    };
    let mut prefix = &content[..span.start];
    if !prefix.is_empty() {
        prefix = prefix.strip_suffix('\n').unwrap_or(prefix);
        if span.marker.nl {
            prefix = prefix.strip_suffix('\n').unwrap_or(prefix);
        }
    }
    let out = format!("{prefix}{}", &content[span.end..]);
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

fn render_drop_in(marker: &Marker, comment: Comment, body: &str) -> String {
    format!(
        "{}\n{}\n",
        marker.render(comment),
        body.trim_end_matches('\n')
    )
}

// ── Registries ────────────────────────────────────────────────────────────────

/// `--registry type=name`, parsed.
pub type RegistryPicks = BTreeMap<String, String>;

pub fn parse_picks(raw: &[String]) -> Result<RegistryPicks> {
    raw.iter()
        .map(|p| {
            p.split_once('=')
                .filter(|(t, n)| !t.is_empty() && !n.is_empty())
                .map(|(t, n)| (t.to_owned(), n.to_owned()))
                .ok_or_else(|| anyhow!("--registry takes <type>=<name>, got {p:?}"))
        })
        .collect()
}

/// A registry can stand in for a public upstream only in `proxy` or `hybrid`
/// mode: a `local` one serves only what was published to it, and pointing
/// `~/.npmrc` at it breaks every public install.
fn is_candidate(r: &RegistryInfo) -> bool {
    matches!(r.mode.as_str(), "proxy" | "hybrid")
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Resolution<'a> {
    One(&'a RegistryInfo),
    None,
    Ambiguous(Vec<String>),
}

/// The registry of `registry_type` a client should use: the `--registry`
/// pick, else the only candidate, else the one candidate a marker on this
/// machine already names.
fn resolve<'a>(
    registry_type: &str,
    registries: &'a [RegistryInfo],
    picks: &RegistryPicks,
    marked: &BTreeSet<String>,
) -> Result<Resolution<'a>> {
    let candidates: Vec<&RegistryInfo> = registries
        .iter()
        .filter(|r| r.registry_type == registry_type && is_candidate(r))
        .collect();
    if let Some(name) = picks.get(registry_type) {
        return candidates
            .iter()
            .find(|r| &r.name == name)
            .map(|r| Resolution::One(r))
            .ok_or_else(|| {
                anyhow!("--registry {registry_type}={name}: the server lists no proxy or hybrid {registry_type} registry of that name")
            });
    }
    Ok(match candidates.as_slice() {
        [] => Resolution::None,
        [one] => Resolution::One(one),
        many => {
            let named: Vec<&&RegistryInfo> =
                many.iter().filter(|r| marked.contains(&r.name)).collect();
            match named.as_slice() {
                [one] => Resolution::One(one),
                _ => Resolution::Ambiguous(many.iter().map(|r| r.name.clone()).collect()),
            }
        }
    })
}

/// The URL clients are pointed at, after the host check of RFC 0038 §4.5:
/// a `public_url` must be on the server's host or one of its subdomains.
fn base_url(r: &RegistryInfo, server: &str, allow_foreign_hosts: bool) -> Result<String> {
    let base = r.base_url(server);
    let host = host_of(&base);
    let server_host = host_of(server);
    if !allow_foreign_hosts && host != server_host && !host.ends_with(&format!(".{server_host}")) {
        bail!(
            "registry {} advertises {base}, which is not on {server_host}; \
             pass --allow-foreign-hosts to write it anyway",
            r.name
        );
    }
    Ok(base)
}

/// Refuse a plain-HTTP server outside loopback: the registry list is the only
/// thing `apply` trusts, and over HTTP anyone on the path rewrites it.
pub fn check_server_scheme(server: &str, insecure_http: bool) -> Result<()> {
    let url =
        reqwest::Url::parse(server).with_context(|| format!("invalid server URL {server:?}"))?;
    let loopback = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    );
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
    pub shell_rc: ShellRc,
    pub only: Vec<ClientId>,
    pub skip: Vec<ClientId>,
    pub allow_foreign_hosts: bool,
}

impl Options<'_> {
    fn selects(&self, c: ClientId) -> bool {
        (self.only.is_empty() || self.only.contains(&c)) && !self.skip.contains(&c)
    }
}

/// What `apply` would do to one file.
#[derive(Debug, Clone, Serialize)]
pub struct TargetPlan {
    pub target: TargetId,
    pub path: PathBuf,
    pub clients: Vec<ClientId>,
    pub state: State,
    pub registries: Vec<String>,
    /// The conflicting source, or the candidates of an ambiguous type.
    pub detail: Option<String>,
    /// The snippet to merge by hand, for a conflict.
    pub snippet: Option<String>,
    #[serde(skip)]
    pub current: Option<String>,
    /// The file's content after `apply`; `None` deletes it. Only meaningful for
    /// `absent`, `drifted` and `orphaned`.
    #[serde(skip)]
    pub next: Option<String>,
    /// Variables this target exports (env.sh), for `setup env`.
    #[serde(skip)]
    pub env: Vec<(String, String)>,
}

impl TargetPlan {
    pub fn writes(&self) -> bool {
        matches!(self.state, State::Absent | State::Drifted | State::Orphaned)
    }
}

/// Every target with a managed part, read once so [`resolve`] can keep the
/// picks the markers record.
fn marked_registries(env: &Env) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for id in ALL_TARGETS {
        let Some(t) = id.target(env) else { continue };
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

/// Registry types the mise drop-in carries rules for: the ones whose URLs mise
/// fetches itself. `generic` needs the upstreams the registry list does not
/// carry yet (RFC 0038 phase 2).
const MISE_TYPES: [&str; 5] = ["github", "forgejo", "npm", "cargo", "openvsx"];

/// The plan for every file, from the registry list and the machine.
pub fn plan(registries: &[RegistryInfo], opts: &Options, env: &Env) -> Result<Vec<TargetPlan>> {
    let marked = marked_registries(env);
    let mut out = Vec::new();
    let mut env_vars: Vec<(String, String)> = Vec::new();
    let mut env_regs: Vec<String> = Vec::new();

    for id in ALL_TARGETS {
        let Some(t) = id.target(env) else { continue };
        let clients: Vec<ClientId> = id
            .clients(env)
            .into_iter()
            .filter(|c| opts.selects(*c))
            .collect();
        if clients.is_empty() {
            continue;
        }
        let installed: Vec<ClientId> = clients
            .iter()
            .copied()
            .filter(|c| env.installed.contains(c))
            .collect();

        let current = match std::fs::read_to_string(&t.path) {
            Ok(s) => Some(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e).with_context(|| format!("reading {}", t.path.display())),
        };
        // The fish drop-in is only a candidate under fish; elsewhere it shows
        // up only when one is already there to take out.
        if id == TargetId::Fish && env.shell.as_deref() != Some("fish") && current.is_none() {
            continue;
        }
        let content = current.as_deref().unwrap_or("");
        let (managed, user) = match t.shape {
            Shape::Block => {
                let span = find_block(content, t.comment).with_context(|| {
                    format!("{}: fix it by hand or run `setup remove`", t.path.display())
                })?;
                (span.map(|s| s.marker), user_part(content, t.comment)?)
            }
            Shape::DropIn => match drop_in_marker(content, t.comment) {
                Some(m) => (Some(m), String::new()),
                None => (None, content.to_owned()),
            },
        };

        let mut p = TargetPlan {
            target: id,
            path: t.path.clone(),
            clients: if installed.is_empty() {
                clients.clone()
            } else {
                installed.clone()
            },
            state: State::NotInstalled,
            registries: Vec::new(),
            detail: None,
            snippet: None,
            current: current.clone(),
            next: None,
            env: Vec::new(),
        };
        if installed.is_empty() {
            out.push(p);
            continue;
        }

        // ── which registries ──
        let types: Vec<&str> = match id {
            TargetId::Cargo => vec!["cargo"],
            TargetId::Npmrc | TargetId::Yarnrc => vec!["npm"],
            TargetId::Pip | TargetId::Uv => vec!["pypi"],
            TargetId::Go => vec!["goproxy"],
            TargetId::Maven => vec!["maven"],
            TargetId::Mise => MISE_TYPES.to_vec(),
            TargetId::EnvSh | TargetId::Fish | TargetId::ShellRc => installed
                .iter()
                .map(|c| {
                    if *c == ClientId::Rustup {
                        "rustup"
                    } else {
                        "nodedist"
                    }
                })
                .collect(),
        };
        let mut chosen: Vec<(&str, &RegistryInfo)> = Vec::new();
        let mut ambiguous: Vec<String> = Vec::new();
        for ty in &types {
            match resolve(ty, registries, &opts.picks, &marked)? {
                Resolution::One(r) => chosen.push((ty, r)),
                Resolution::None => {}
                Resolution::Ambiguous(names) => {
                    ambiguous.push(format!("{ty}: {}", names.join(", ")));
                }
            }
        }
        p.registries = chosen.iter().map(|(_, r)| r.name.clone()).collect();
        p.registries.dedup();

        if !ambiguous.is_empty() {
            p.state = State::Ambiguous;
            p.detail = Some(format!(
                "several candidates — pick with --registry <type>=<name> ({})",
                ambiguous.join("; ")
            ));
            out.push(p);
            continue;
        }

        // ── the body ──
        let mut marker = Marker {
            registries: p.registries.clone(),
            ..Marker::default()
        };
        let body = match id {
            TargetId::EnvSh | TargetId::Fish | TargetId::ShellRc => {
                if id == TargetId::EnvSh {
                    for (ty, r) in &chosen {
                        let base = base_url(r, opts.server, opts.allow_foreign_hosts)?;
                        env_vars.extend(env_vars_for(ty, &base));
                    }
                    env_regs.clone_from(&p.registries);
                }
                p.env.clone_from(&env_vars);
                p.registries.clone_from(&env_regs);
                marker.registries.clone_from(&env_regs);
                if env_vars.is_empty() {
                    None
                } else {
                    match id {
                        TargetId::EnvSh => Some(render_env(&env_vars, EnvShell::Sh)),
                        TargetId::Fish
                            if opts.shell_rc == ShellRc::Auto
                                && env.shell.as_deref() == Some("fish") =>
                        {
                            Some(render_env(&env_vars, EnvShell::Fish))
                        }
                        TargetId::ShellRc if opts.shell_rc == ShellRc::Auto => {
                            let path = shell_quote(&env_sh_path(env).to_string_lossy());
                            Some(format!("[ -r {path} ] && . {path}"))
                        }
                        _ => None,
                    }
                }
            }
            TargetId::Mise if chosen.is_empty() => None,
            TargetId::Mise => {
                let mut body = String::from("[settings.url_replacements]\n");
                for (ty, r) in &chosen {
                    let base = base_url(r, opts.server, opts.allow_foreign_hosts)?;
                    for (pattern, replacement) in typed_url_replacements(ty) {
                        push_replacement(
                            &mut body,
                            pattern,
                            &replacement.replace("{proxy}", &base),
                        );
                    }
                }
                Some(body)
            }
            _ => match chosen.first() {
                None => None,
                Some(&(_, r)) => {
                    let base = base_url(r, opts.server, opts.allow_foreign_hosts)?;
                    if id == TargetId::Maven {
                        let mirror_of = opts
                            .maven_mirror_of
                            .map(|m| m.mirror_of().to_owned())
                            .or_else(|| managed.as_ref().and_then(|m| m.mirror_of.clone()))
                            .unwrap_or_else(|| "central".to_owned());
                        marker.mirror_of = Some(mirror_of.clone());
                        Some(render_maven(&r.name, &base, &mirror_of))
                    } else {
                        Some(render_body(id, &base))
                    }
                }
            },
        };

        let Some(body) = body else {
            p.state = if managed.is_some() {
                State::Orphaned
            } else {
                State::NoRegistry
            };
            if managed.is_some() {
                p.next = match t.shape {
                    Shape::DropIn => None,
                    Shape::Block => unsplice(content, t.comment)?,
                };
            }
            out.push(p);
            continue;
        };

        // ── a setting the user owns ──
        let rc_user = rc_user_part(env)?;
        if let Some(why) = conflict(
            id,
            &t,
            current.is_some() && managed.is_none(),
            &user,
            env,
            &rc_user,
            &p.env,
        )?
        .or_else(|| yaml_key_clash(&t.path, &user, &body))
        {
            p.state = State::Conflict;
            p.detail = Some(why);
            p.snippet = Some(body);
            out.push(p);
            continue;
        }

        let next = match t.shape {
            Shape::DropIn => {
                marker.created = true;
                render_drop_in(&marker, t.comment, &body)
            }
            Shape::Block => splice(current.as_deref(), t.comment, marker, &body)?,
        };
        p.state = match (&current, managed.is_some()) {
            (Some(c), true) if *c == next => State::UpToDate,
            (_, true) => State::Drifted,
            (_, false) => State::Absent,
        };
        p.next = Some(next);
        out.push(p);
    }
    Ok(out)
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

/// The rc file's own lines, for the `export VAR=` override scan.
fn rc_user_part(env: &Env) -> Result<String> {
    let Some(path) = rc_path(env) else {
        return Ok(String::new());
    };
    match std::fs::read_to_string(&path) {
        Ok(c) => user_part(&c, Comment::Hash),
        Err(_) => Ok(String::new()),
    }
}

/// A setting outside the block that the block would fight: in the file
/// itself (RFC 0038 §4.3, first table), or one that overrides the file
/// (second table).
fn conflict(
    id: TargetId,
    t: &Target,
    unmanaged_file: bool,
    user: &str,
    env: &Env,
    rc_user: &str,
    env_out: &[(String, String)],
) -> Result<Option<String>> {
    let path = t.path.display();
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
        TargetId::Pip => lines().any(|l| l == "[global]").then(|| {
            format!("{path} has a [global] section: add the index-url line below to it by hand")
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
            (default_index || doc.contains_key("index-url"))
                .then(|| format!("{path} already sets a default index"))
        }
        TargetId::Go => lines()
            .any(|l| l.starts_with("GOPROXY="))
            .then(|| format!("{path} already sets GOPROXY")),
        TargetId::Maven | TargetId::Mise | TargetId::EnvSh | TargetId::Fish => {
            unmanaged_file.then(|| format!("{path} exists and was not written by batlehub setup"))
        }
        TargetId::ShellRc => None,
    };
    if in_file.is_some() {
        return Ok(in_file);
    }

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
            .find(|n| exported(n).is_some())
            .map(|n| format!("${n} is set and overrides {path}"))
    };
    Ok(match id {
        TargetId::Cargo => {
            let legacy = env.cargo_home.join("config");
            if legacy.is_file() {
                Some(format!(
                    "{} exists and cargo reads it instead of {path}",
                    legacy.display()
                ))
            } else {
                env.vars
                    .keys()
                    .find(|k| k.starts_with("CARGO_REGISTRIES_") || k.starts_with("CARGO_SOURCE_"))
                    .map(|k| format!("${k} is set and overrides {path}"))
            }
        }
        TargetId::Npmrc => var(&["NPM_CONFIG_REGISTRY", "npm_config_registry"]),
        TargetId::Yarnrc => var(&["YARN_NPM_REGISTRY_SERVER"]),
        TargetId::Pip => var(&["PIP_INDEX_URL", "PIP_CONFIG_FILE"]),
        TargetId::Uv => var(&["UV_INDEX_URL", "UV_DEFAULT_INDEX"]),
        TargetId::Go => var(&["GOPROXY"]),
        TargetId::EnvSh => env_out.iter().find_map(|(k, want)| {
            exported(k)
                .filter(|have| have != want)
                .map(|have| format!("${k} is already set to {have}"))
        }),
        _ => None,
    })
}

// ── Bodies ────────────────────────────────────────────────────────────────────

/// The block for a single-registry target. The URL shapes are the console's
/// (`ui/src/config/registryTypes.ts`), which the 0005-bis drift gate guards.
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
            host_of(base)
        ),
        TargetId::Yarnrc => format!("npmRegistryServer: \"{base}/\""),
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

/// The variables an env-only registry needs, as the console prints them.
fn env_vars_for(registry_type: &str, base: &str) -> Vec<(String, String)> {
    let pairs: Vec<(&str, String)> = match registry_type {
        "rustup" => vec![
            ("RUSTUP_DIST_SERVER", format!("{base}/rustup")),
            ("RUSTUP_UPDATE_ROOT", format!("{base}/rustup/rustup")),
        ],
        "nodedist" => {
            let reg = format!("{base}/nodedist");
            vec![
                ("NVM_NODEJS_ORG_MIRROR", reg.clone()),
                ("FNM_NODE_DIST_MIRROR", reg.clone()),
                ("N_NODE_MIRROR", reg.clone()),
                ("NODEJS_ORG_MIRROR", reg),
            ]
        }
        _ => vec![],
    };
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
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

/// Single-quote for sh and fish alike; a URL never carries a `'`, but a home
/// directory might.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// ── Writing ───────────────────────────────────────────────────────────────────

/// Write `content` to `path` atomically — a sibling temp file, fsync, rename —
/// keeping the original's mode. A symlink (a dotfile manager's) is written
/// through to its target, never replaced by a regular file.
pub fn write_atomic(path: &Path, content: &str) -> Result<()> {
    let path = if path.is_symlink() {
        std::fs::canonicalize(path).with_context(|| format!("resolving {}", path.display()))?
    } else {
        path.to_owned()
    };
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("{} has no file name", path.display()))?
        .to_string_lossy();
    let tmp = dir.join(format!(".{name}.batlehub-tmp"));
    let result = (|| -> Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
        if let Ok(meta) = std::fs::metadata(&path) {
            std::fs::set_permissions(&tmp, meta.permissions())?;
        }
        std::fs::rename(&tmp, &path)?;
        Ok(())
    })();
    if result.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    result.with_context(|| format!("writing {}", path.display()))
}

/// Delete `path` (through a symlink, the link itself goes: it pointed at a
/// file only `apply` had created).
fn delete(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("deleting {}", path.display())),
    }
}

/// Apply one plan entry: write its next content, or delete the file.
pub fn apply_one(p: &TargetPlan) -> Result<()> {
    match &p.next {
        Some(content) => write_atomic(&p.path, content),
        None => delete(&p.path),
    }
}

/// What `remove` does to one file.
#[derive(Debug, Clone, Serialize)]
pub struct Removal {
    pub path: PathBuf,
    /// `None`: the file is deleted.
    #[serde(skip)]
    pub next: Option<String>,
    pub deletes: bool,
    #[serde(skip)]
    pub current: String,
}

/// Every managed part on this machine, and what the file becomes without it.
/// Needs no server: the markers say everything.
pub fn removals(env: &Env, only: &[ClientId], skip: &[ClientId]) -> Result<Vec<Removal>> {
    let selects = |c: &ClientId| (only.is_empty() || only.contains(c)) && !skip.contains(c);
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for id in ALL_TARGETS {
        if !id.clients(env).iter().any(selects)
            && !(id == TargetId::Yarnrc && selects(&ClientId::Yarn))
        {
            continue;
        }
        let Some(t) = id.target(env) else { continue };
        let paths: Vec<PathBuf> = if id == TargetId::ShellRc {
            all_rc_paths(env).to_vec()
        } else {
            vec![t.path.clone()]
        };
        for path in paths {
            if !seen.insert(path.clone()) {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let next = match t.shape {
                Shape::DropIn if drop_in_marker(&content, t.comment).is_some() => None,
                Shape::DropIn => continue,
                Shape::Block => {
                    if find_block(&content, t.comment)
                        .with_context(|| path.display().to_string())?
                        .is_none()
                    {
                        continue;
                    }
                    unsplice(&content, t.comment)?
                }
            };
            out.push(Removal {
                deletes: next.is_none(),
                path,
                next,
                current: content,
            });
        }
    }
    Ok(out)
}

pub fn remove_one(r: &Removal) -> Result<()> {
    match &r.next {
        Some(content) => write_atomic(&r.path, content),
        None => delete(&r.path),
    }
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
        }
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
            registries: vec!["npm1".into()],
            ..Marker::default()
        };
        let spliced = splice(Some(original), Comment::Hash, marker, "registry=x/").unwrap();
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
        let first = splice(Some("a=1"), Comment::Hash, m.clone(), "x=1").unwrap();
        let edited = format!("{first}after=1\n");
        let second = splice(Some(&edited), Comment::Hash, m, "x=2").unwrap();
        assert!(second.contains("x=2") && !second.contains("x=1"));
        assert!(second.ends_with("after=1\n"));
        // `nl` survives the rewrite, so remove still restores the original.
        assert_eq!(
            unsplice(&second, Comment::Hash).unwrap().unwrap(),
            "a=1after=1\n"
        );
    }

    #[test]
    fn a_created_file_is_deleted_by_unsplice() {
        let s = splice(None, Comment::Hash, Marker::default(), "x=1").unwrap();
        assert_eq!(unsplice(&s, Comment::Hash).unwrap(), None);
        // An empty file that existed is kept, empty.
        let s = splice(Some(""), Comment::Hash, Marker::default(), "x=1").unwrap();
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
            registries: vec!["a".into(), "b".into()],
            created: true,
            nl: true,
            mirror_of: Some("external:*".into()),
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
        // The registry is gone: orphaned, and apply takes the block out.
        let none: Vec<RegistryInfo> = vec![];
        let orphan = plan(&none, &opts(), &env).unwrap();
        let p = find(&orphan, TargetId::Npmrc);
        assert_eq!(p.state, State::Orphaned);
        apply_one(p).unwrap();
        assert!(
            !dir.path().join(".npmrc").exists(),
            "apply created it, so it goes"
        );
    }

    #[test]
    fn local_registries_are_not_candidates() {
        let regs = vec![reg("pub", "npm", "proxy"), reg("priv", "npm", "local")];
        assert_eq!(
            resolve("npm", &regs, &RegistryPicks::new(), &BTreeSet::new()).unwrap(),
            Resolution::One(&regs[0])
        );
        let only_local = vec![reg("priv", "npm", "local")];
        assert_eq!(
            resolve("npm", &only_local, &RegistryPicks::new(), &BTreeSet::new()).unwrap(),
            Resolution::None
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
                "[[index]]\nurl = \"x\"\n",
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
        assert_eq!(find(&plans, TargetId::ShellRc).state, State::NoRegistry);
    }

    #[test]
    fn foreign_and_plain_http_hosts_are_refused() {
        let mut r = reg("n", "npm", "proxy");
        r.public_url = Some("https://npm.hub.example.com".into());
        assert!(base_url(&r, SERVER, false).is_ok());
        r.public_url = Some("https://evil.example.org".into());
        assert!(base_url(&r, SERVER, false).is_err());
        assert!(base_url(&r, SERVER, true).is_ok());
        // `hub.example.com.evil.org` is not a subdomain.
        r.public_url = Some("https://xhub.example.com".into());
        assert!(base_url(&r, SERVER, false).is_err());

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

        for r in removals(&env, &[], &[]).unwrap() {
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
        assert!(removals(&env, &[], &[]).unwrap().is_empty());
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

        write_atomic(&link, "a=2\n").unwrap();
        assert!(link.is_symlink());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "a=2\n");
        assert_eq!(
            std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn diff_shows_only_the_span() {
        let d = diff(Path::new("f"), "a\nb\n", Some("a\nx\nb\n"));
        assert!(d.contains("+x\n") && !d.contains("-a") && !d.contains("+a"));
    }
}
