use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// A config file this process was started with: the primary, or one of its
/// overlays. Frozen at construction into the pieces every caller needs, so that
/// no part of the reload service holds a config path as a bare `String`.
///
/// **What this type is for.** `ConfigReloadService` reads a path on two request
/// paths (`load_pending`, `config_content`) and rewrites one on a third
/// (`persist_config_to_disk`). Today every one of them resolves to the process's
/// own `--config` argument, which is not attacker input — but "today" is the
/// whole of the guarantee, and it rests on nobody ever assigning the field from
/// somewhere else. This type moves that from a review obligation to a property
/// of the code: the only constructor is [`ConfigFile::from_process_argument`],
/// it is `pub(super)`, and it is called in exactly one place —
/// `ConfigReloadService::new`, with the string the process was started with. A
/// future feature that wants to let a *request* choose a config file cannot
/// reach these sinks by assigning a `String`; it has to add a constructor here,
/// next to this comment, which is where the traversal check would then belong.
///
/// It does **not** make the value safe by itself, and it does not clear the
/// standing `rust/path-injection` alerts — those name the handler's
/// `web::Data` parameter as their source, and taint rides `&self` onto every
/// field whatever its type (`docs/internal/codeql-triage-2026-09-19.md`).
///
/// **The split is a consolidation, not a bug fix.** `persist_config_to_disk`
/// used to re-derive the directory and the file name from the raw string on
/// every write, to place its temp file; the rename target and both readers used
/// the raw string as given, so they already agreed. Deriving once, here, means
/// the temp file and the file it replaces cannot come from two different
/// readings of the same string — worth having, but nothing was broken.
///
/// It does normalise the arguments for which `Path::file_name` is `None` —
/// `..`, `/`, `""`, `a/..`, `./` — from "read the raw path, stage a temp file
/// next to a `config.toml` that was never read" to "read and write
/// `dir/config.toml`". None of them can reach this constructor: `load_layered`
/// reads the same path at startup and fails first. (A *trailing slash* is not
/// one of these cases — `Path::new("/etc/batlehub/").file_name()` is
/// `Some("batlehub")`, so that argument was always consistent.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConfigFile {
    /// `dir` joined with `name`, precomputed: the one path that is ever opened.
    path: PathBuf,
    /// The directory the file lives in, for siblings — `persist_config_to_disk`
    /// stages its temp file here, because a rename across a mount point fails
    /// with `EXDEV`.
    ///
    /// `Path::parent` is `Some("")` for a bare relative name like `config.toml`,
    /// and joining onto `""` yields the name unchanged, which is the current
    /// directory — where the target lives too.
    dir: PathBuf,
    /// The file's own name, always a single normal component: `Path::file_name`
    /// yields `None` rather than a separator or a `..` — verified for the
    /// degenerate arguments in this module's tests, because it is what keeps
    /// `dir.join(name)` inside `dir` and the temp file named after it likewise.
    name: OsString,
}

impl ConfigFile {
    /// The only constructor, named for its only legitimate caller.
    ///
    /// `raw` is a `--config` argument (or a `BATLEHUB_CONFIG` segment, or the
    /// `"config.toml"` default) as `server/src/main.rs`'s `config_paths`
    /// produced it. Anything else reaching here is the defect this type exists
    /// to make visible.
    ///
    /// Infallible on purpose: it runs after `load_layered` has already read
    /// every one of these paths, so an argument that names no file has failed
    /// startup long before. The `"config.toml"` fallback is the one
    /// `persist_config_to_disk` already applied to such an argument, kept
    /// rather than reasoned about — it is unreachable either way.
    pub(super) fn from_process_argument(raw: &str) -> Self {
        let given = Path::new(raw);
        let dir = given
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf();
        let name = given
            .file_name()
            .unwrap_or_else(|| OsStr::new("config.toml"))
            .to_os_string();
        Self {
            path: dir.join(&name),
            dir,
            name,
        }
    }

    /// The file itself — the only path this service opens.
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    /// The directory to stage a sibling temp file in.
    pub(super) fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file's own name, for naming that temp file after it.
    pub(super) fn name(&self) -> &OsStr {
        &self.name
    }

    /// Read it. Every read of a config file in this service goes through here.
    pub(super) async fn read(&self) -> std::io::Result<String> {
        tokio::fs::read_to_string(&self.path).await
    }
}

impl std::fmt::Display for ConfigFile {
    /// The effective path, so a log line names the file that was actually
    /// opened rather than the argument it was derived from.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.path.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_relative_name_stays_in_the_current_directory() {
        let f = ConfigFile::from_process_argument("config.toml");
        assert_eq!(f.path(), Path::new("config.toml"));
        assert_eq!(f.dir(), Path::new(""));
        assert_eq!(f.name(), OsStr::new("config.toml"));
    }

    #[test]
    fn an_absolute_path_keeps_its_directory() {
        let f = ConfigFile::from_process_argument("/etc/batlehub/config.toml");
        assert_eq!(f.path(), Path::new("/etc/batlehub/config.toml"));
        assert_eq!(f.dir(), Path::new("/etc/batlehub"));
        assert_eq!(f.name(), OsStr::new("config.toml"));
    }

    /// A `..` an operator typed is theirs to type — this type freezes the path,
    /// it does not police where an operator may keep their own config.
    #[test]
    fn a_relative_parent_in_the_operators_argument_is_preserved() {
        let f = ConfigFile::from_process_argument("../cfg/prod.toml");
        assert_eq!(f.path(), Path::new("../cfg/prod.toml"));
        assert_eq!(f.dir(), Path::new("../cfg"));
    }

    /// The property the write path depends on: whatever the argument, the name
    /// is one component, so `dir.join(name)` cannot climb out of `dir` and the
    /// temp file named after it cannot either.
    #[test]
    fn the_name_is_always_a_single_component() {
        for raw in [
            "config.toml",
            "/etc/batlehub/config.toml",
            "../cfg/prod.toml",
            "./a/b/c.toml",
            "/etc/batlehub/",
            "..",
            "/",
        ] {
            let f = ConfigFile::from_process_argument(raw);
            let n = Path::new(f.name());
            assert_eq!(
                n.components().count(),
                1,
                "{raw}: name {n:?} is not one component"
            );
            assert!(
                matches!(n.components().next(), Some(std::path::Component::Normal(_))),
                "{raw}: name {n:?} is not a normal component"
            );
        }
    }

    /// A trailing slash is *not* a path without a file name — `file_name()`
    /// normalises it away — so this argument resolves to the file an operator
    /// means by it, not to a `config.toml` inside it. Pinned because the
    /// opposite is the intuitive reading and it is wrong.
    #[test]
    fn a_trailing_slash_names_the_file_not_a_directory() {
        let f = ConfigFile::from_process_argument("/etc/batlehub/");
        assert_eq!(f.path(), Path::new("/etc/batlehub"));
        assert_eq!(f.dir(), Path::new("/etc"));
        assert_eq!(f.name(), OsStr::new("batlehub"));
    }

    /// The arguments that really have no file name, and the fallback they get.
    /// All five fail startup in `load_layered` before this type sees them; the
    /// test exists so the fallback is a decision on record rather than an
    /// `unwrap_or` nobody has evaluated.
    #[test]
    fn an_argument_naming_no_file_falls_back_to_config_toml() {
        for (raw, expected) in [
            ("..", "config.toml"),
            ("/", "config.toml"),
            ("", "config.toml"),
            ("a/..", "a/config.toml"),
            ("./", "config.toml"),
        ] {
            let f = ConfigFile::from_process_argument(raw);
            assert_eq!(f.path(), Path::new(expected), "for {raw:?}");
            assert_eq!(f.dir().join(f.name()), f.path(), "for {raw:?}");
        }
    }

    /// The invariant the whole type is for: the path opened is always the two
    /// frozen components joined, never a string re-parsed at the call site.
    #[test]
    fn the_path_is_always_its_two_components() {
        for raw in [
            "config.toml",
            "/etc/batlehub/config.toml",
            "../cfg/prod.toml",
            "/etc/batlehub/",
            "..",
        ] {
            let f = ConfigFile::from_process_argument(raw);
            assert_eq!(f.dir().join(f.name()), f.path(), "for {raw:?}");
        }
    }

    #[test]
    fn display_is_the_effective_path() {
        assert_eq!(
            ConfigFile::from_process_argument("/etc/batlehub/config.toml").to_string(),
            "/etc/batlehub/config.toml"
        );
    }
}
