//! The shared runner for binary scanners (RFC 0018 §6.3, §7): every
//! invocation of `postmortem`, `guarddog` or `trivy` goes through here, and
//! through `bwrap`.
//!
//! # The invariant this enforces
//!
//! The artifact is attacker-controlled input, and the worker is the one
//! process that opens it while holding database and storage credentials. So
//! a scanner runs with: new user, pid, ipc and uts namespaces; **no network**
//! unless the scanner declares it needs one; the root filesystem bind-mounted
//! read-only; the per-job work directory as the only writable mount, and
//! that one `nosuid`/`nodev`; `--die-with-parent` and `--new-session`; an
//! empty environment but for `HOME` and `PATH`; and the rlimits of
//! `[worker.sandbox]`. argv is passed directly — there is no shell anywhere
//! in this path — and stdout is read as **untrusted data**: capped in size,
//! parsed as JSON under a strict schema, never interpolated into anything.
//!
//! `runtime = "none"` runs the bare command and exists for unit tests and
//! for the `BATLEHUB_UNSAFE_NO_SANDBOX=1` escape hatch config validation
//! guards; the argv it would have handed `bwrap` is still built and tested,
//! so the sandbox is exercised on every machine even where `bwrap` is not.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use batlehub_core::ports::ScannerError;
use tokio::io::AsyncReadExt;

/// Stdout above this is a scanner that is not answering the question.
pub const STDOUT_CAP_BYTES: usize = 32 * 1024 * 1024;

/// What `[worker.sandbox]` decided (RFC 0018 §4.1).
#[derive(Debug, Clone)]
pub struct Sandbox {
    /// `bwrap` or `none`.
    pub runtime: String,
    /// The `bwrap` binary, when not on `PATH`.
    pub bwrap: PathBuf,
    pub memory_limit_mb: u64,
    pub cpu_seconds: u64,
}

impl Default for Sandbox {
    fn default() -> Self {
        Self {
            runtime: "bwrap".to_owned(),
            bwrap: PathBuf::from("bwrap"),
            memory_limit_mb: 2048,
            cpu_seconds: 300,
        }
    }
}

/// One scanner invocation, as the scanner describes it.
#[derive(Debug, Clone)]
pub struct Invocation {
    pub command: PathBuf,
    pub args: Vec<String>,
    /// The per-job directory: the only writable mount, and the cwd.
    pub work_dir: PathBuf,
    /// Whether the sandbox keeps the network namespace (`postmortem` with
    /// `online = true`, the timeline, `trivy` against a server).
    pub needs_network: bool,
    pub timeout: Duration,
}

/// What came back.
#[derive(Debug)]
pub struct Output {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr_tail: String,
}

/// The argv `bwrap` is given for `inv`, exactly (RFC 0018 §6.3). Public so
/// the tests can assert it without a `bwrap` on the machine. The rlimits of
/// the sandbox are not `bwrap` flags — they are set on the child before
/// exec by [`run`] — which is why `sandbox` is not read here.
pub fn bwrap_argv(_sandbox: &Sandbox, inv: &Invocation) -> Vec<String> {
    let work = inv.work_dir.to_string_lossy().into_owned();
    let mut argv: Vec<String> = vec![
        "--unshare-user".into(),
        "--unshare-pid".into(),
        "--unshare-ipc".into(),
        "--unshare-uts".into(),
    ];
    if !inv.needs_network {
        argv.push("--unshare-net".into());
    }
    argv.extend([
        "--die-with-parent".into(),
        "--new-session".into(),
        // The rootfs, read-only; the scanner's own binary and its runtime
        // come from here. `/proc` and `/dev` are the minimal ones `bwrap`
        // synthesises, not the host's.
        "--ro-bind".into(),
        "/".into(),
        "/".into(),
        "--proc".into(),
        "/proc".into(),
        "--dev".into(),
        "/dev".into(),
        // The one writable mount: the work directory, mounted over itself.
        "--bind".into(),
        work.clone(),
        work.clone(),
        "--chdir".into(),
        work.clone(),
        // An empty environment but for the two things a binary needs to
        // find itself and a place to write.
        "--clearenv".into(),
        "--setenv".into(),
        "HOME".into(),
        work,
        "--setenv".into(),
        "PATH".into(),
        "/usr/local/bin:/usr/bin:/bin".into(),
        "--".into(),
    ]);
    argv.push(inv.command.to_string_lossy().into_owned());
    argv.extend(inv.args.iter().cloned());
    argv
}

/// The `prlimit`-style limits applied around the sandbox: address space and
/// CPU time, from `[worker.sandbox]`. Applied by the runner through `ulimit`
/// semantics on the child (`setrlimit` before exec), not by `bwrap`, which
/// has no flag for them.
fn apply_rlimits(cmd: &mut tokio::process::Command, sandbox: &Sandbox) {
    let mem = sandbox.memory_limit_mb.saturating_mul(1024 * 1024);
    let cpu = sandbox.cpu_seconds;
    // SAFETY: `setrlimit` is async-signal-safe and touches nothing but the
    // calling process's own limits; the closure runs in the forked child
    // before `exec`, where that is exactly what is wanted.
    unsafe {
        cmd.pre_exec(move || {
            let set = |resource: libc::__rlimit_resource_t, value: u64| {
                let lim = libc::rlimit {
                    rlim_cur: value as libc::rlim_t,
                    rlim_max: value as libc::rlim_t,
                };
                // A limit the process may not raise is not an error worth
                // failing the scan over: the sandbox's other walls stand.
                let _ = libc::setrlimit(resource, &lim);
            };
            if mem > 0 {
                set(libc::RLIMIT_AS, mem);
            }
            if cpu > 0 {
                set(libc::RLIMIT_CPU, cpu);
            }
            Ok(())
        });
    }
}

/// Run `inv` under `sandbox`, bounded by its timeout and the stdout cap.
///
/// A non-zero exit is **not** an error here: `postmortem` exits 1 when its
/// own gate trips, which is an answer, not a failure. The caller decides
/// from `status` and the parsed output; a timeout, a signal, or a stdout
/// above the cap is a [`ScannerError`].
pub async fn run(sandbox: &Sandbox, inv: &Invocation) -> Result<Output, ScannerError> {
    let (program, args): (PathBuf, Vec<String>) = if sandbox.runtime == "none" {
        (inv.command.clone(), inv.args.clone())
    } else {
        (sandbox.bwrap.clone(), bwrap_argv(sandbox, inv))
    };
    let mut cmd = tokio::process::Command::new(&program);
    cmd.args(&args)
        .current_dir(&inv.work_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if sandbox.runtime == "none" {
        // The sandbox would have cleared it; without one, the scanner still
        // gets a clean environment rather than the worker's.
        cmd.env_clear()
            .env("HOME", &inv.work_dir)
            .env("PATH", std::env::var("PATH").unwrap_or_default());
    }
    apply_rlimits(&mut cmd, sandbox);

    let mut child = cmd.spawn().map_err(|e| {
        ScannerError::Crashed(format!("could not start {}: {e}", program.display()))
    })?;
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");

    // stderr is drained on its own task so a chatty scanner cannot block on
    // a full pipe; stdout is read here, and a cap overrun kills the child
    // rather than leaving it blocked on a pipe nobody reads any more.
    let stderr_task = tokio::spawn(async move {
        let mut err = Vec::new();
        // Past the cap the tail is what is kept, so the reader keeps
        // draining rather than failing: stderr is diagnostics, not data.
        let mut buf = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut buf).await {
            if n == 0 {
                break;
            }
            err.extend_from_slice(&buf[..n]);
            if err.len() > 64 * 1024 {
                let keep = err.len() - 32 * 1024;
                err.drain(..keep);
            }
        }
        err
    });
    let read_stdout = async {
        let mut out = Vec::new();
        read_capped(&mut stdout, &mut out, STDOUT_CAP_BYTES).await?;
        let status = child
            .wait()
            .await
            .map_err(|e| ScannerError::Crashed(e.to_string()))?;
        Ok::<_, ScannerError>((status, out))
    };
    let (status, out) = match tokio::time::timeout(inv.timeout, read_stdout).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            let _ = child.kill().await;
            stderr_task.abort();
            return Err(e);
        }
        Err(_) => {
            let _ = child.kill().await;
            stderr_task.abort();
            return Err(ScannerError::Timeout);
        }
    };
    let err = stderr_task.await.unwrap_or_default();
    let stderr_tail = String::from_utf8_lossy(&err)
        .chars()
        .rev()
        .take(2000)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    match status.code() {
        Some(code) => Ok(Output {
            status: Some(code),
            stdout: out,
            stderr_tail,
        }),
        // Killed by a signal: the sandbox's rlimits, or the OOM killer.
        None => Err(ScannerError::Crashed(format!(
            "{} was killed by a signal ({stderr_tail})",
            inv.command.display()
        ))),
    }
}

async fn read_capped<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    into: &mut Vec<u8>,
    cap: usize,
) -> Result<(), ScannerError> {
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .await
            .map_err(|e| ScannerError::Output(e.to_string()))?;
        if n == 0 {
            return Ok(());
        }
        if into.len() + n > cap {
            return Err(ScannerError::Output(format!(
                "scanner output exceeded {cap} bytes"
            )));
        }
        into.extend_from_slice(&buf[..n]);
    }
}

/// Parse scanner stdout as JSON — hostile data, so a size cap has already
/// applied and the caller matches against a strict shape.
pub fn parse_json(stdout: &[u8]) -> Result<serde_json::Value, ScannerError> {
    serde_json::from_slice(stdout)
        .map_err(|e| ScannerError::Output(format!("scanner output is not JSON: {e}")))
}

/// A fresh per-job work directory under the system temp dir, removed on drop.
pub fn work_dir(prefix: &str) -> Result<tempfile::TempDir, ScannerError> {
    tempfile::Builder::new()
        .prefix(&format!("batlehub-{prefix}-"))
        .tempdir()
        .map_err(|e| ScannerError::Other(format!("creating a work directory: {e}")))
}

/// Whether `path` is a file this process could execute — what `[scanners]`
/// validation and the startup check ask about a `command`.
pub fn command_exists(path: &Path) -> bool {
    if path.components().count() == 1 {
        // A bare name: on PATH?
        return std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|dir| dir.join(path).is_file()));
    }
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inv(needs_network: bool) -> Invocation {
        Invocation {
            command: PathBuf::from("/usr/local/bin/postmortem"),
            args: vec!["scan".into(), ".".into(), "--json".into()],
            work_dir: PathBuf::from("/tmp/batlehub-job-1"),
            needs_network,
            timeout: Duration::from_secs(5),
        }
    }

    /// RFC 0018 §6.3, line by line: every namespace, no network, read-only
    /// root, one writable mount, an empty environment, no shell.
    #[test]
    fn the_bwrap_argv_is_the_sandbox_the_rfc_describes() {
        let argv = bwrap_argv(&Sandbox::default(), &inv(false));
        for flag in [
            "--unshare-user",
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
            "--unshare-net",
            "--die-with-parent",
            "--new-session",
            "--clearenv",
        ] {
            assert!(argv.iter().any(|a| a == flag), "{flag} missing: {argv:?}");
        }
        let ro = argv.iter().position(|a| a == "--ro-bind").unwrap();
        assert_eq!(&argv[ro + 1..ro + 3], ["/", "/"]);
        let bind = argv.iter().position(|a| a == "--bind").unwrap();
        assert_eq!(
            &argv[bind + 1..bind + 3],
            ["/tmp/batlehub-job-1", "/tmp/batlehub-job-1"]
        );
        let sep = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(
            &argv[sep + 1..],
            ["/usr/local/bin/postmortem", "scan", ".", "--json"],
            "argv is passed directly, no shell"
        );
        assert!(!argv.iter().any(|a| a.contains("sh -c")));
    }

    #[test]
    fn a_scanner_that_needs_the_network_keeps_the_namespace() {
        let argv = bwrap_argv(&Sandbox::default(), &inv(true));
        assert!(!argv.iter().any(|a| a == "--unshare-net"));
        assert!(
            argv.iter().any(|a| a == "--unshare-pid"),
            "everything else stays"
        );
    }

    #[tokio::test]
    async fn runtime_none_runs_the_bare_command_and_reports_its_exit_code() {
        let dir = work_dir("test").unwrap();
        let sandbox = Sandbox {
            runtime: "none".into(),
            ..Sandbox::default()
        };
        let out = run(
            &sandbox,
            &Invocation {
                command: PathBuf::from("/bin/sh"),
                args: vec!["-c".into(), "echo '{\"ok\":true}'; exit 1".into()],
                work_dir: dir.path().to_path_buf(),
                needs_network: false,
                timeout: Duration::from_secs(5),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            out.status,
            Some(1),
            "a non-zero exit is an answer, not an error"
        );
        assert_eq!(parse_json(&out.stdout).unwrap()["ok"], true);
    }

    #[tokio::test]
    async fn a_scanner_that_hangs_times_out() {
        let dir = work_dir("test").unwrap();
        let sandbox = Sandbox {
            runtime: "none".into(),
            ..Sandbox::default()
        };
        let err = run(
            &sandbox,
            &Invocation {
                command: PathBuf::from("/bin/sh"),
                args: vec!["-c".into(), "sleep 30".into()],
                work_dir: dir.path().to_path_buf(),
                needs_network: false,
                timeout: Duration::from_millis(200),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ScannerError::Timeout), "{err}");
    }

    #[tokio::test]
    async fn stdout_above_the_cap_is_an_output_error_not_an_answer() {
        let dir = work_dir("test").unwrap();
        let sandbox = Sandbox {
            runtime: "none".into(),
            ..Sandbox::default()
        };
        let mut inv = inv(false);
        inv.command = PathBuf::from("/bin/sh");
        inv.args = vec!["-c".into(), "head -c 40000000 /dev/zero".into()];
        inv.work_dir = dir.path().to_path_buf();
        inv.timeout = Duration::from_secs(20);
        let err = run(&sandbox, &inv).await.unwrap_err();
        assert!(matches!(err, ScannerError::Output(_)), "{err}");
    }

    #[test]
    fn a_missing_command_is_reported_as_such() {
        assert!(!command_exists(Path::new("/nonexistent/postmortem")));
        assert!(command_exists(Path::new("sh")));
        assert!(command_exists(Path::new("/bin/sh")));
    }
}
